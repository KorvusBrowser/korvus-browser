//! The MODULE LOADER — `import` resolution, fetching, and the deferred module queue.
//!
//! Moved out of `lib.rs` VERBATIM (the file was 4,222 lines; this is the module-loading
//! third of it). Nothing in this file was edited in the move: every line here can be diffed against
//! its old position and comes back identical, which is the property that makes a split reviewable.
//!
//! Three things live here and they are one subject:
//!
//! * **The queue.** A `<script type=module>` is DEFERRED — a browser evaluates it after the document
//!   parses and before `DOMContentLoaded`. `script_exec.rs` hands each one to [`queue_module`]; the
//!   drain is `event_loop.rs`'s, because it is the event loop that owns the await.
//! * **The loader.** [`KorvusModuleLoader`] resolves a specifier against the importing module's URL,
//!   fetches it over the kernel's transport wearing the page's persona headers
//!   ([`ModuleFetchState`]), and classifies the response.
//! * **What a response IS.** [`KnownModuleType`] plus the MIME rules under it: HTML's JavaScript MIME
//!   essence list is a FIXED list, `is_json_mime_type` follows the JSON-suffix rule, and a `data:`
//!   URL is decoded here rather than fetched.

use deno_core::{op2, resolve_import, ModuleCodeString, ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleResolveResponse, ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, OpState, ResolutionKind};

/// Total wall-clock a single drain of the deferred-module queue may spend loading (i.e. fetching every
/// static import) and evaluating, across all of a document's module scripts.
///
/// A bound is mandatory, not defensive: `run_event_loop` resolves only when nothing at all is
/// scheduled, and this runtime keeps a perpetual timer alive on purpose so a page's `setInterval` work
/// keeps running — the same fact that makes [`Engine::pump`] necessary. It is small enough to stay
/// well under a harness watchdog (the conformance runner's is 4s, and the classic-script fetches have
/// already spent some of that).
///
/// **IT IS ONE DEADLINE FOR THE WHOLE DRAIN, AND THAT IS NOW THE RIGHT SHAPE.** It was
/// not, while the drain loaded each queued module's graph IN TURN: what a single deadline then
/// bounded was the SUM of every module script's import fetches, so a page with a dozen module
/// scripts spent the whole 2 s on the first few and every later one timed out having waited for
/// nothing. Bracketed by IM1 on a local server that delays each import by a fixed amount, one
/// import per module:
///
/// ```text
///                          korvus, before MD1                      korvus, after   chrome 150
///   3 modules x 300 ms     all three loaded                        all three       all three
///   3 modules x 700 ms     two loaded, the THIRD timed out         all three       all three
///   6 modules x 700 ms     two loaded, the other FOUR timed out    all six         all six
/// ```
///
/// That was the whole of the live corpus's `module <url> load module: timed out fetching imports`
/// cluster — 6 rows at `chrome_rows = 0`. It is NOT the bare-specifier defect IM1 fixed: one of
/// those six pages carries no import map at all, and two of them still emit 13 and 18 of these
/// after every bare specifier on them resolves. The counts also move run to run (3, 5, 13, 18 on
/// repeat loads of the same pages) because what they measure is network latency against a fixed
/// wall clock, which is why the cluster carries `unstable` verdicts.
///
/// **WHAT CHANGED IS THE SERIALIZATION, NOT THE NUMBER, and that was the point.** Making the
/// deadline per-module is one line and it would let a page with a dozen module scripts spend a
/// dozen budgets, which blows the watchdog this bound exists to stay under; a navigation-wide
/// budget wide enough for six 700 ms graphs in series is the same widening wearing a second
/// constant's clothes. Instead `drain_modules` now WARMS every queued module's static imports
/// before it loads any of them ([`crate::Engine::warm_queued_module_imports`]), through this
/// document's own module map, so independent graphs are in flight together and this deadline bounds
/// **the slowest graph** rather than the sum of all of them. Chrome's own behaviour, measured:
/// six imports leave within 7 ms of each other and six 700 ms graphs cost 0.93 s, not 4.2 s
/// (`tests/module_drain_live.rs` carries the readings and the transcript both engines print).
///
/// **THE WORST CASE A DRAIN ADMITS IS THEREFORE UNCHANGED at 2,000 ms of waiting**, which is what
/// lets this stay under every caller's deadline: the WPT runner's 8,000 ms per-file watchdog with
/// its 2,500 ms pump loop, and CDP's `pump_until_loaded(1,500 ms)`. Chrome bounds module import
/// fetching nowhere at all — 30 s is served and run, and a module that never answers costs the
/// document its `DOMContentLoaded` indefinitely (measured at 6 s and 20 s). This engine cannot
/// follow it there, because its callers must return; the divergence is recorded in that test rather
/// than papered over with a bigger number.
pub(crate) const MODULE_DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_millis(2000);

/// Deferred ES-module scripts (`<script type="module">`) awaiting async evaluation. Browser semantics:
/// module scripts are **deferred** — parsed at load, evaluated after the document parses during the event
/// loop. `load_html`/`load_page` are sync at the collection point and cannot `await`, so they queue module
/// sources here; `run()` drains + evaluates them (async `load_*_es_module_from_code` + `mod_evaluate`)
/// before pumping the loop.
///
/// THE TYPES ARE `webapi`'s.
/// The queue now has TWO producers: this file, for the `<script type=module>`s the PARSER found, and
/// `webapi`'s inserted-script path, for the ones the PAGE inserted — an inline one from
/// `run_inserted_script` and an external one from the element loader's completion. It still has ONE
/// consumer, `Engine::evaluate_pending_modules`. Since the dependency edge points korvus-runtime ->
/// korvus-webapi, a queue declared here could not be filled from there, and the only ways to keep it
/// here were a second queue or a second evaluator — both of which the brief forbids and both of which
/// would have made "a module script evaluates exactly once, in the drain's order" a coincidence.
///
/// `PendingModule`'s own doc (see `webapi::page`) carries the two-URL rule: for an
/// external module script the name and the import base are one thing, and for an INLINE one Chrome
/// makes them two — on a page carrying `<base href="/sub/">`, `Error.stack` names the DOCUMENT url
/// while `import.meta.url` and every relative import resolve against the BASE url.
pub(crate) use webapi::page::{PendingModule, PendingModules};

/// What an http(s) module fetch wears on the wire. The loader is built before the engine knows which
/// page it will load, and a `ModuleLoader` gets no `OpState`, so `load_page` writes the current
/// navigation's persona headers + initiator here and the loader reads them at fetch time. Shared by
/// `Rc` with the engine; never crosses a thread (deno_core's module futures are `!Send` by design).
#[derive(Default)]
pub(crate) struct ModuleFetchState {
    /// The persona this document's imports are fetched AS: its identity headers (UA + client hints,
    /// the same set a `<script src>` carries) and the wire they go out on, as one value. `None`
    /// before a navigation has set it, which is also the only state in which a module fetch is
    /// refused for naming no persona.
    pub(crate) identity: Option<transport::PersonaIdentity>,
    /// The document that owns these imports — Fetch's *initiator*, which is what makes `Sec-Fetch-Site`
    /// come out `same-origin` instead of `none`.
    pub(crate) initiator: Option<String>,
    /// The importing document's referrer policy: its `<meta name=referrer>`, else its
    /// response's `Referrer-Policy`.
    pub(crate) referrer_policy: Option<transport::ReferrerPolicy>,
    /// `(inline module name, the base URL its imports resolve against)` for the current document.
    /// Every inline module of one document shares both halves by construction —
    /// `run_page_scripts` computes the base once and names them all after the one document URL — so
    /// one pair is the whole truth, not a sample of it.
    ///
    /// It has to OUTLIVE the load, which is why it is navigation state and not an argument: static
    /// `import`s are resolved while the module compiles, but `import.meta.resolve("./x.js")` is
    /// resolved whenever page script calls it, arbitrarily later. A stale pair from a previous
    /// document is inert rather than wrong — its name cannot match a referrer in the current one —
    /// and `load_page` overwrites it before each drain.
    pub(crate) inline_base: Option<(String, String)>,
    /// The document's import map, shared by `Rc` with `OpState` so the two places that
    /// can install one -- the page-script driver, for the maps the PARSER found, and `webapi`'s
    /// inserted-script path, for the ones the PAGE injected -- write into the very store this
    /// loader reads. It has to be a shared cell rather than a value handed over at drain time,
    /// because a map can arrive AFTER the drain: Chrome 150 applies one injected from script, and
    /// `import()` from a classic script resolves through this loader at any later moment.
    pub(crate) import_maps: std::rc::Rc<std::cell::RefCell<webapi::page::ImportMapStore>>,
    /// The realm's `blob:` URL bytes, shared with `OpState`'s registry: `import(blobURL)` loads
    /// what the page minted, and a revoked or never-minted URL fails as a network error does.
    pub(crate) blobs: Option<webapi::BlobBytes>,
    /// THE DOCUMENT'S MODULE MAP, by module URL. HTML's *fetch a single module script* puts the
    /// entry in the map BEFORE it goes to the network, in state "fetching", and a second import of
    /// that URL waits on that entry instead of starting a second fetch — which is why
    /// `for (…) import('./x.js')` is one request and not eight. The entry then stays for the life
    /// of the document, in HTML as here: a module map is never evicted, and a FAILED fetch is a
    /// null entry that makes the next import fail without asking the network again.
    ///
    /// MEASURED, stock Chrome 150.0.7871.46 against a loopback server answering `Cache-Control:
    /// no-store` (so no cache can be doing the work): eight concurrent `import('./m.mjs')` are ONE
    /// request in Chrome and were EIGHT here, and five `await import()` in series are ONE. Astro's
    /// islands are exactly the first shape — `proton.me` has 53 of them awaiting one
    /// `before-hydration.js`, and this engine asked for it 26 times on every replay.
    ///
    /// WHY IT IS NOT LEFT TO deno_core, which also refuses to `load()` a module it has already
    /// registered: registration happens after the source is compiled and instantiated, so between
    /// a first import's response and its registration there is a window in which a second import
    /// finds nothing and fetches again. That window is exactly the one a loaded machine widens,
    /// and a rule that holds only on an idle box is not the rule Chrome implements.
    ///
    /// `navigation.rs` clears it when the next document starts; nothing here outlives a navigation.
    pub(crate) fetched: std::collections::HashMap<String, SharedModuleFetch>,
    /// Module url -> the url of the `import()` whose graph it is in. Chrome rejects a
    /// dynamic import whose graph fails anywhere with "Failed to fetch dynamically imported module:
    /// <that ROOT url>", and a static import inside the graph resolves with the STATIC wording; deno_core
    /// hands both kinds of resolution over as `DynamicImport`, so the graph is told apart here by its
    /// referrers. A module shared by two graphs is reported under the latest one to reach it.
    pub(crate) dyn_roots: std::collections::HashMap<String, String>,
    /// The modules a dynamic graph is loading RIGHT NOW: their source has come back and
    /// deno_core is about to resolve their static imports -- which it does synchronously, as it
    /// registers the module, and with `ResolutionKind::DynamicImport` like the graph's own root. A
    /// `DynamicImport` resolution whose referrer is here is therefore a static import inside the graph;
    /// any other one is an `import()` CALL, a root of its own (Chrome 150 names that call's url, even
    /// from a module an earlier graph shared or loaded: `c9/repro/dynroot`). Emptied by `finish_load`,
    /// which deno_core runs as each load ends -- by then every module of it has been registered.
    pub(crate) dyn_loading: std::collections::HashSet<String>,
    /// The module fetches this loader has finished, for the realm's resource timeline (shared with `OpState`;
    /// `webapi::drain_module_fetches` puts them there).
    pub(crate) timings: webapi::ModuleFetches,
}

/// One in-flight module fetch, awaitable by every import that asked for it. The response is split
/// into its three parts rather than kept as a `ModuleSource`, because each waiter must build its
/// own: `ModuleSource` is not `Clone`, and the MIME rules below are decided per REQUESTED type, so
/// two imports of one URL with different import attributes get their own answer off one response.
pub(crate) type SharedModuleFetch =
    deno_core::futures::future::Shared<std::pin::Pin<Box<dyn std::future::Future<Output = Result<FetchedModule, String>>>>>;

/// A module response, in the three parts the rules below read.
#[derive(Clone)]
pub(crate) struct FetchedModule {
    status: u16,
    headers: std::rc::Rc<Vec<(String, String)>>,
    body: std::rc::Rc<str>,
}

/// Korvus's ES-module loader. `resolve` is spec-compliant (relative/absolute via `resolve_import`);
/// `load` satisfies `data:` URL modules inline (the self-contained form `import ... from 'data:...'`)
/// and fetches `http:`/`https:` modules over the stealth transport with the page's own headers.
/// Modules handed directly to `load_*_es_module_from_code` never hit `load()` for themselves — only
/// their imports do.
///
/// WHY http AT ALL. Deferring `<script type=module>` to the module queue is only half a module
/// implementation: the queue evaluated bodies at a synthetic specifier, against which
/// `import {x} from "./helpers.mjs"` cannot resolve, so any module that imported anything died on its
/// first line. Giving each queued module its real URL is what makes relative resolution meaningful,
/// and a real URL is only useful if something can then fetch it.
pub(crate) struct KorvusModuleLoader {
    pub(crate) fetch: std::rc::Rc<std::cell::RefCell<ModuleFetchState>>,
}

impl KorvusModuleLoader {
    /// Spec resolution of `specifier` against the referrer, with the ONE substitution this engine
    /// makes: an inline module script is NAMED after the document URL so that a stack frame reads the
    /// way Chrome's does, but HTML says its imports resolve against the document's BASE url, and
    /// `<base href>` makes those two different strings. Shared by `resolve` and
    /// `import_meta_resolve`, which agree on the URL and differ only on what may be refused.
    fn resolve_against_base(&self, specifier: &str, referrer: &str, how: Worded) -> Result<ModuleSpecifier, deno_error::JsErrorBox> {
        let base = self.fetch.borrow().inline_base.as_ref().and_then(|(name, base)| (name == referrer).then(|| base.clone()));
        let referrer = base.as_deref().unwrap_or(referrer);
        resolve_from_base(&self.fetch, specifier, referrer, how)
    }
}

/// HTML's "resolve a module specifier", against a base URL the caller has already chosen. Split out
/// of [`KorvusModuleLoader::resolve_against_base`] so that the drain's PREFETCH
/// ([`crate::Engine::warm_queued_module_imports`]) resolves through the very same rule rather than a
/// second copy of it — a prefetch that resolved differently from the loader would warm a URL nothing
/// asks for, which is a request the page never made.
///
/// The one thing the caller owns is the base: the loader derives it from the referrer plus the
/// inline-module substitution, while the prefetch already holds the queued module's
/// own `import_base` and passes it straight in.
pub(crate) fn resolve_from_base(
    fetch: &std::rc::Rc<std::cell::RefCell<ModuleFetchState>>,
    specifier: &str,
    referrer: &str,
    how: Worded,
) -> Result<ModuleSpecifier, deno_error::JsErrorBox> {
    // THE IMPORT MAP COMES FIRST, and it is asked about every specifier, not only the bare ones:
    // a map remaps `./relative-a.js` and `https://example.invalid/x.js` as readily as `react`
    // (measured, Chrome 150). The referrer is the one the inline-base swap above produced, which
    // is also what a `scopes` prefix is matched against -- HTML matches the importing script's
    // BASE url, and `<base href>` moves it.
    //
    // A referrer that is not a URL is deno_core's root sentinel `"."`, which no map can scope
    // and no relative specifier can resolve against; the ordinary path below reports it.
    if let Ok(referrer_url) = url::Url::parse(referrer) {
        match fetch.borrow().import_maps.borrow_mut().resolve(specifier, &referrer_url) {
            Ok(Some(url)) => return Ok(url),
            // The map had nothing to say: a URL-like specifier with no entry resolves the
            // ordinary way, which is the same answer by the same parser.
            Ok(None) => {}
            Err(e) => return Err(deno_error::JsErrorBox::type_error(how.render(&e))),
        }
    }
    resolve_import(specifier, referrer).map_err(deno_error::JsErrorBox::from_err)
}

/// THE DOCUMENT'S MODULE-MAP ENTRY for `url`, created and put in the map if this is the first import
/// to ask for it (see [`ModuleFetchState::fetched`] for why the entry is taken BEFORE the network).
///
/// Two callers, and the second: the loader, on the import deno_core is
/// currently resolving, and [`crate::Engine::warm_queued_module_imports`], on every static import of
/// every module script already on the drain's queue. Because the entry is the same `Shared` future
/// either way, a warmed fetch is not a second request — it is the ONE request, started earlier, and
/// the loader finds it in flight and awaits it.
///
/// Returns `None` for a URL this loader does not fetch over the network (`data:` and anything else);
/// the caller has nothing to warm there.
pub(crate) fn module_map_entry(
    fetch: &std::rc::Rc<std::cell::RefCell<ModuleFetchState>>,
    url: &ModuleSpecifier,
) -> Option<SharedModuleFetch> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    let key = url.as_str().to_string();
    if let Some(s) = fetch.borrow().fetched.get(&key) {
        return Some(s.clone());
    }
    let (identity, initiator, policy, timings) = {
        let st = fetch.borrow();
        (st.identity.clone(), st.initiator.clone(), st.referrer_policy, st.timings.clone())
    };
    let s = deno_core::futures::FutureExt::shared(
        Box::pin(fetch_module(key.clone(), identity, initiator, policy, timings))
            as std::pin::Pin<Box<dyn std::future::Future<Output = Result<FetchedModule, String>>>>,
    );
    fetch.borrow_mut().fetched.insert(key, s.clone());
    Some(s)
}

/// WHICH OF THE THREE SENTENCES a resolution failure is reported with. One failure, three strings,
/// and the difference is not cosmetic -- a corpus row is clustered by its message, so a lane that
/// reports the static wording for a dynamic `import()` moves a row into a cluster it is not in.
/// Measured on Chrome 150, 2026-09-21; the table is in `webapi`'s `core/html/import_map.rs`.
#[derive(Clone, Copy)]
pub(crate) enum Worded {
    /// A static `import` — double quotes, then the explanation.
    StaticImport,
    /// A dynamic `import()` — single quotes, and NO explanation. Chrome really does drop it here.
    DynamicImport,
    /// `import.meta.resolve` — no quotes, a colon, then the explanation.
    MetaResolve,
}

impl Worded {
    fn render(self, e: &webapi::page::ImportMapResolveError) -> String {
        match self {
            Self::StaticImport => e.static_message(),
            Self::DynamicImport => e.dynamic_message(),
            Self::MetaResolve => e.meta_resolve_message(),
        }
    }
}

impl deno_core::ModuleLoader for KorvusModuleLoader {
    /// Resolve `specifier` against the referrer's base URL.
    ///
    /// The ONE substitution: an inline module script is named after the document URL, so
    /// that a stack frame reads the way Chrome's does, but HTML says its imports resolve against the
    /// document's BASE url — and `<base href>` makes those two different strings. When the referrer
    /// is that inline module, the base is swapped in — in `resolve_against_base`, which
    /// `import_meta_resolve` below shares, so the two always agree on the URL.
    ///
    /// An external module never matches: it is named after its own src, and a document URL is HTML
    /// rather than a module, so nothing else can be resolving against this name.
    ///
    /// THE SECOND HALF IS THE PRICE OF THE FIRST, PAID BACK. Naming an inline module after the
    /// document URL puts that URL in deno_core's `by_name` map, so `import("./self.html")` from the
    /// page would find the inline module there and resolve to its namespace. Chrome has no such entry
    /// (an inline module is never in the module map) and instead FETCHES the document URL and fails on
    /// it -- measured: `REJECTED: Failed to fetch dynamically imported module`, where korvus answered
    /// `RESOLVED: []`. deno_core cannot hide a map entry, so the import is refused here instead, which
    /// is the same outcome by the only route available. `import_meta_resolve` below deliberately does
    /// NOT take this branch, because `import.meta.resolve` only computes a URL and Chrome hands back
    /// the string without fetching anything.
    ///
    /// `ROOT_LOAD_REFERRER` IS WHY THE GUARD IS CONDITIONAL, and leaving it out breaks every inline
    /// module rather than some edge case: `RecursiveModuleLoad`'s root step resolves the module being
    /// loaded against the literal referrer `"."` (`recursive_load.rs`, `LoadInit::Side =>
    /// module_map_rc.resolve(specifier, ".", ResolutionKind::Import)`), so the FIRST thing asked of
    /// this function for an inline module is to resolve that module's own name. `_kind` cannot tell
    /// the two apart -- a root side-load and a real static import are both `Import` -- but the
    /// referrer can, because `"."` is deno_core's sentinel for "no referrer, this is the root" and a
    /// real import's referrer is always a module URL.
    fn resolve(&self, specifier: &str, referrer: &str, kind: ResolutionKind) -> ModuleResolveResponse {
        const ROOT_LOAD_REFERRER: &str = ".";
        // `kind` is read for one thing only: which of Chrome's three
        // wordings a failed resolution is reported with. It still cannot tell a root side-load from a
        // real static import -- both are `Import` -- which is what the referrer below is for.
        // A `DynamicImport` resolution is a static import inside a graph only while its referrer is
        // being loaded by that graph (H37, H37c -- see `ModuleFetchState::dyn_loading`): the static
        // wording, and the graph's root carried down. Otherwise it is an `import()` call, and the root
        // of its own graph.
        let parent_root = match kind {
            ResolutionKind::DynamicImport => {
                let f = self.fetch.borrow();
                f.dyn_loading.contains(referrer).then(|| f.dyn_roots.get(referrer).cloned()).flatten()
            }
            _ => None,
        };
        let how = match (kind, &parent_root) {
            (ResolutionKind::DynamicImport, None) => Worded::DynamicImport,
            _ => Worded::StaticImport,
        };
        let resolved = self.resolve_against_base(specifier, referrer, how)?;
        if matches!(kind, ResolutionKind::DynamicImport) {
            let mut f = self.fetch.borrow_mut();
            // The LATEST graph to reach a module owns it: a graph's resolutions run just before its
            // loads, so a module shared with an earlier `import()` is reported under the one now failing
            // (Chrome: `import("./deep.js")` whose `./nested404.js` was imported on its own before still
            // names deep.js).
            let root = parent_root.unwrap_or_else(|| resolved.to_string());
            f.dyn_roots.insert(resolved.to_string(), root);
        }
        let is_inline_module_name = referrer != ROOT_LOAD_REFERRER
            && self.fetch.borrow().inline_base.as_ref().is_some_and(|(name, _)| resolved.as_str() == name);
        if is_inline_module_name {
            return Err(deno_error::JsErrorBox::type_error(format!(
                "Failed to fetch dynamically imported module: {resolved}"
            )));
        }
        Ok(resolved)
    }

    /// `import.meta.resolve(x)`: the URL, and nothing else. deno_core's default delegates to
    /// [`Self::resolve`], which would refuse a specifier that lands on the inline module's own name --
    /// right for an `import`, wrong here, since Chrome resolves the string happily and only fails if
    /// something later tries to FETCH it.
    fn import_meta_resolve(&self, specifier: &str, referrer: &str) -> Result<ModuleSpecifier, deno_error::JsErrorBox> {
        self.resolve_against_base(specifier, referrer, Worded::MetaResolve)
    }

    /// A module load has ended (deno_core runs this as each one is dropped): every module it loaded
    /// is registered, with its static imports resolved, so none of them is "loading" any more -- an
    /// `import()` one of them makes from here on is a root of its own.
    fn finish_load(&self) {
        self.fetch.borrow_mut().dyn_loading.clear();
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        maybe_referrer: Option<&ModuleLoadReferrer>,
        options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let spec = module_specifier.as_str();
        // A failure anywhere in an `import()`'s graph is reported as Chrome does (H37): one TypeError
        // naming the ROOT import's url, whatever failed and wherever.
        // An `import()`'s own module is its root; any other module of the graph takes its REFERRER's
        // root, read now -- deno_core does not resolve again the imports of a module it compiled for an
        // earlier graph, so a dependency reached through it again carries only the old root.
        let dyn_root = options.is_dynamic_import.then(|| {
            let f = self.fetch.borrow();
            match f.dyn_roots.get(spec) {
                Some(r) if r == spec => r.clone(),
                own => maybe_referrer
                    .and_then(|r| f.dyn_roots.get(r.specifier.as_str()))
                    .or(own)
                    .cloned()
                    .unwrap_or_else(|| spec.to_string()),
            }
        });
        let dyn_failure = |fallback: String| -> deno_error::JsErrorBox {
            match &dyn_root {
                Some(root) => deno_error::JsErrorBox::type_error(format!("Failed to fetch dynamically imported module: {root}")),
                None => deno_error::JsErrorBox::type_error(fallback),
            }
        };
        // WHAT THE IMPORT ASKED FOR. `import v from "./x.json" with { type: "json" }` reaches the
        // loader as `RequestedModuleType::Json`, and until this line the loader ignored it and
        // answered `ModuleType::JavaScript` for everything — so V8 handed a JSON body to the JS
        // parser and the file died on `Unexpected token ':'`, and a CSS body died on
        // `Private field '#test' must be declared in an enclosing class`, which is a `#test {}`
        // selector being read as a class field. Both messages are lies about what happened.
        let want = match KnownModuleType::classify(&options.requested_module_type) {
            Ok(w) => w,
            Err(ty) => {
                // An import attribute this engine cannot honour must fail HERE, naming itself, rather
                // than fall through to the JS parser and blame the file's syntax.
                return ModuleLoadResponse::Sync(Err(deno_error::JsErrorBox::type_error(format!(
                    "Failed to load module {spec}: import attribute type \"{ty}\" is not supported"
                ))));
            }
        };
        if let Some(code) = decode_data_url_js(spec) {
            // A `data:` module carries its bytes in the URL, so there is no Content-Type to check
            // beyond the one the URL states; the requested type decides how it is interpreted.
            if options.is_dynamic_import {
                self.fetch.borrow_mut().dyn_loading.insert(spec.to_string());
            }
            return ModuleLoadResponse::Sync(Ok(ModuleSource::new(
                want.module_type(),
                ModuleSourceCode::String(ModuleCodeString::from(code)),
                module_specifier,
                None,
            )));
        }
        if module_specifier.scheme() == "blob" {
            let key = spec.split('#').next().unwrap_or(spec);
            // This realm's own URLs first, then any other realm's in the process (a worker importing its
            // page's blob URL), as `webapi::blob_url_bytes` answers the other loaders.
            let found = self.fetch.borrow().blobs.as_ref().and_then(|b| b.borrow().get(key).cloned()).or_else(|| webapi::shared_blob_bytes(key));
            let Some((ty, bytes)) = found else {
                return ModuleLoadResponse::Sync(Err(dyn_failure(format!(
                    "Failed to fetch dynamically imported module: {spec}"
                ))));
            };
            let headers: Vec<(String, String)> = if ty.is_empty() { Vec::new() } else { vec![("content-type".to_string(), ty.clone())] };
            if want == KnownModuleType::JavaScript
                && !webapi::page::script_response_ok(webapi::page::ScriptFlavor::Module, 200, &headers)
            {
                return ModuleLoadResponse::Sync(Err(dyn_failure(format!(
                    "Failed to load module script {spec}: not a JavaScript MIME type"
                ))));
            }
            if want == KnownModuleType::Json && !is_json_mime_type(&ty) {
                return ModuleLoadResponse::Sync(Err(deno_error::JsErrorBox::type_error(format!(
                    "Failed to load module {spec}: expected a JSON MIME type but got \"{ty}\""
                ))));
            }
            if options.is_dynamic_import {
                self.fetch.borrow_mut().dyn_loading.insert(spec.to_string());
            }
            return ModuleLoadResponse::Sync(Ok(ModuleSource::new(
                want.module_type(),
                ModuleSourceCode::String(ModuleCodeString::from(String::from_utf8_lossy(&bytes).into_owned())),
                module_specifier,
                None,
            )));
        }
        if module_specifier.scheme() == "http" || module_specifier.scheme() == "https" {
            let url = spec.to_string();
            let owned = module_specifier.clone();
            // THE MODULE MAP ENTRY (see [`ModuleFetchState::fetched`]). Taken BEFORE the network, so
            // every import that resolves to this URL -- while the fetch is in flight and after it
            // has settled -- gets the one response instead of issuing its own request. The drain's
            // prefetch takes the SAME entry through the same function, which is what
            // makes an early warm one request rather than two.
            let Some(shared) = module_map_entry(&self.fetch, module_specifier) else {
                return ModuleLoadResponse::Sync(Err(deno_error::JsErrorBox::generic(format!(
                    "Korvus module loader: unsupported specifier `{spec}` (data:, http: and https: load)"
                ))));
            };
            let dyn_root = dyn_root.clone();
            // Marked LOADING when the source comes back, not now: a concurrent load ending meanwhile
            // empties the set (H37c), and deno_core resolves this module's imports as it registers it,
            // right after this future answers.
            let loading = options.is_dynamic_import.then(|| self.fetch.clone());
            return ModuleLoadResponse::Async(Box::pin(async move {
                // In an `import()`'s graph every failure below is Chrome's one sentence (H37).
                let fail_dyn = || dyn_root.as_ref().map(|root| deno_error::JsErrorBox::type_error(format!("Failed to fetch dynamically imported module: {root}")));
                // `KORVUS_TRACE_MODULES`: what H37's one sentence replaces -- the load's own failure, by url.
                let trace = |what: &str| {
                    if std::env::var_os("KORVUS_TRACE_MODULES").is_some() {
                        eprintln!("[module] {url} (root {}) {what}", dyn_root.as_deref().unwrap_or("-"));
                    }
                };
                let resp = match shared.await {
                    Ok(r) => r,
                    Err(e) => {
                        trace(&format!("FETCH FAILED: {e}"));
                        return Err(fail_dyn().unwrap_or_else(|| deno_error::JsErrorBox::generic(e)));
                    }
                };
                trace(&format!("<- {}", resp.status));
                if resp.status >= 400 {
                    // A 404 helper must REJECT the import, exactly as a browser does, rather than
                    // instantiate an empty module and fail later with a confusing binding error.
                    return Err(fail_dyn().unwrap_or_else(|| {
                        deno_error::JsErrorBox::generic(format!("failed to fetch module {url}: HTTP {}", resp.status))
                    }));
                }
                // A JAVASCRIPT module needs a JavaScript MIME type -- Chrome's "Strict
                // MIME type checking is enforced for module scripts", measured for text/plain,
                // text/html, application/json, application/octet-stream and no Content-Type at all,
                // every one refused. It is the same rule the parser-inserted module path applies to
                // its own response, so it is asked of the same function.
                if want == KnownModuleType::JavaScript
                    && !webapi::page::script_response_ok(webapi::page::ScriptFlavor::Module, resp.status, &resp.headers)
                {
                    trace(&format!("REFUSED: not a JavaScript MIME type ({:?})", resp.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type"))));
                    return Err(fail_dyn().unwrap_or_else(|| {
                        deno_error::JsErrorBox::type_error(format!("Failed to load module script {url}: not a JavaScript MIME type"))
                    }));
                }
                // THE MIME CHECK IS PART OF THE TYPE, not decoration. HTML's "fetch a single module
                // script" refuses a JSON module whose response is not a JSON MIME type, and
                // `json-module/invalid-content-type.any.js` is 100 assertions of precisely the strings
                // that look like JSON and are not. A text module, by contrast, ignores Content-Type
                // entirely — `text-module/ignore-content-type.any.js` asserts every one of the same
                // strings SUCCEEDS. Two different rules, so two different code paths.
                if want == KnownModuleType::Json {
                    // Blink's `IsJSONMimeType(response.HttpContentType())` (`module_script_fetcher.cc:87`): the first
                    // value of the joined field, not the first line whole (A/B C5).
                    let ct = transport::mime::http_content_type(&resp.headers).unwrap_or_default();
                    let ct = ct.as_str();
                    if !is_json_mime_type(ct) {
                        return Err(deno_error::JsErrorBox::type_error(format!(
                            "Failed to load module {url}: expected a JSON MIME type but got \"{ct}\""
                        )));
                    }
                }
                if let Some(fetch) = &loading {
                    fetch.borrow_mut().dyn_loading.insert(url.clone());
                }
                Ok(ModuleSource::new(
                    want.module_type(),
                    ModuleSourceCode::String(ModuleCodeString::from(resp.body.to_string())),
                    &owned,
                    None,
                ))
            }));
        }
        ModuleLoadResponse::Sync(Err(deno_error::JsErrorBox::generic(format!(
            "Korvus module loader: unsupported specifier `{spec}` (data:, http: and https: load)"
        ))))
    }
}

/// The network half of a module load, split out so that every import of one URL can await ONE of
/// them (see [`ModuleFetchState::fetching`]). The error is a `String` and not a `JsErrorBox`
/// because a shared future's output must be `Clone`; each waiter wraps it in its own.
///
/// Destination `script`, mode `cors`: a module script is ALWAYS a CORS request, unlike a classic
/// `<script src>` which is no-cors. That difference is on the wire (`Sec-Fetch-Mode`, and an
/// `Origin` header), so it is set here rather than inherited from the classic path.
///
/// AND IT IS A PAGE REQUEST. `Sec-Fetch-Mode: cors` went out and the `Origin`
/// header did not -- on the cross-origin import as well as the same-origin one -- because this
/// request carried no `page`, and `page` is what subjects a request to tainting, to the CORS check
/// and to the per-hop `Origin`. Measured 2026-09-18 against Brave 151, Chrome 150, Edge 151 and
/// Opera 135: all four send `Origin` on BOTH, and korvus sent it on neither. `cors_preconfigured`
/// is the difference between a module script and a same-origin `fetch()`, which sends none.
///
/// The origin is the importing document's, from the same `initiator` that decides `Sec-Fetch-Site`,
/// so the two cannot name different documents. A non-http initiator (or none) leaves `page` unset,
/// which is the state a module fetched before any navigation is already in.
async fn fetch_module(
    url: String,
    identity: Option<transport::PersonaIdentity>,
    initiator: Option<String>,
    referrer_policy: Option<transport::ReferrerPolicy>,
    timings: webapi::ModuleFetches,
) -> Result<FetchedModule, String> {
    let origin = initiator
        .as_deref()
        .and_then(|s| url::Url::parse(s).ok())
        .filter(|u| u.scheme() == "http" || u.scheme() == "https")
        .map(|u| u.origin().ascii_serialization());
    let mut req = transport::FetchRequest {
        url: url.clone(),
        method: "GET".to_string(),
        headers: Vec::new(),
        body: None,
        destination: transport::Destination::from("script"),
        mode: transport::Mode::Cors,
        initiator,
        user_activated: false,
        wire: None,
        page: origin.map(|origin| transport::PageRequest {
            origin,
            // A module script carries none: the author never gets to set headers on it.
            author_headers: Vec::new(),
            cors_preconfigured: true,
        }),
        referrer_policy,
        ..Default::default()
    };
    // A page module's site for cookies is the page's.
    req.cookie_cross_site = req.initiator.as_deref().is_some_and(|i| transport::cookie_cross_site("", i, &req.url));
    // Headers and wire from the same persona in one call, so an imported module cannot reach the
    // network wearing a different browser than the document that imported it.
    if let Some(id) = identity.as_ref() {
        transport::apply_persona_identity(id, &mut req);
    }
    let resp = transport::fetch(req)
        .await
        .map_err(|e| format!("failed to fetch module {url}: {e}"))?;
    // its resource-timing entry, ONE per request (the module map shares this future among every import of the url)
    timings.0.borrow_mut().push(resp.timing.clone());
    Ok(FetchedModule {
        status: resp.status,
        headers: std::rc::Rc::new(resp.headers),
        body: std::rc::Rc::from(resp.body.as_str()),
    })
}

/// The import-attribute types this engine can actually produce a module for.
///
/// Deliberately NOT a passthrough of [`deno_core::RequestedModuleType`]. deno_core will happily take
/// `ModuleType::Bytes`, and `ModuleType::Other` for anything at all, but a browser answers
/// `with { type: "css" }` with a `CSSStyleSheet` and `with { type: "bananas" }` with a TypeError —
/// so the set this converts is the set this can honour, and everything else is named and refused at
/// the boundary rather than mistranslated deeper in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KnownModuleType {
    JavaScript,
    Json,
    Text,
}

impl KnownModuleType {
    /// `Err(name)` for a requested type this engine will not pretend to satisfy — the name is what the
    /// error message reports. `css` lands here on purpose: see [`KorvusModuleLoader`]'s note.
    fn classify(req: &deno_core::RequestedModuleType) -> Result<Self, String> {
        match req {
            deno_core::RequestedModuleType::None => Ok(Self::JavaScript),
            deno_core::RequestedModuleType::Json => Ok(Self::Json),
            deno_core::RequestedModuleType::Text => Ok(Self::Text),
            other => Err(other.as_str().unwrap_or("unknown").to_string()),
        }
    }

    fn module_type(self) -> ModuleType {
        match self {
            Self::JavaScript => ModuleType::JavaScript,
            Self::Json => ModuleType::Json,
            Self::Text => ModuleType::Text,
        }
    }
}

/// Is `value` — a raw `Content-Type` field value — a **JSON MIME type** in Fetch's sense?
///
/// Fetch defines it as: essence `application/json`, essence `text/json`, or any type whose subtype
/// ends in `+json`. Every part of that sentence is load-bearing, which is why this is a real parse
/// and not a `contains("json")`. `json-module/invalid-content-type.any.js` enumerates the ways a
/// string can look like JSON and not be one, and each of these branches is one of its rows:
///   * `application/json+protobuf` — `+json` must be a SUFFIX of the subtype, not an infix.
///   * `text/x-json` — the subtype must be exactly `json`, not merely end in it.
///   * `text/json/json` — exactly one `/`.
///   * `applic ation/vnd.api+json`, `applic"ation/...`, `aplicación/...` — both halves must be HTTP
///     tokens (RFC 9110 `tchar`), so a space, a separator or a non-ASCII byte disqualifies them.
///   * `text/html;+json` — parameters are cut at the first `;` BEFORE any of this is looked at.
fn is_json_mime_type(value: &str) -> bool {
    // Parameters are not part of the essence. Cut them first: `text/html;+json` must be judged as
    // `text/html`, which is not JSON, and `application/json;charset=utf-8` as `application/json`,
    // which is.
    let essence = value.split(';').next().unwrap_or("");
    // HTTP whitespace only (space and tab) — `\n`/`\r` are NOT trimmable here; they are control
    // characters that must fail the token test, which is what the `applic\x0Aation/...` rows check.
    let essence = essence.trim_matches([' ', '\t']);
    let Some((ty, subtype)) = essence.split_once('/') else {
        return false;
    };
    if subtype.contains('/') || !is_http_token(ty) || !is_http_token(subtype) {
        return false;
    }
    let subtype = subtype.to_ascii_lowercase();
    let ty = ty.to_ascii_lowercase();
    if subtype == "json" {
        return ty == "application" || ty == "text";
    }
    // `+json` as a suffix, and something must precede it: `+json` alone is not a subtype.
    subtype.len() > "+json".len() && subtype.ends_with("+json")
}

/// A non-empty RFC 9110 `token`: one or more `tchar`. Rejects the empty string, anything non-ASCII,
/// every control character, space, and the separator set.
fn is_http_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~')
        })
}

/// Decode a `data:[<mediatype>],<data>` JavaScript module URL to its source (non-base64, percent-encoded
/// form — what browsers accept for `import ... from 'data:text/javascript,...'`). Returns `None` for a
/// non-`data:` URL or a base64 payload (not needed by our fixtures yet).
fn decode_data_url_js(spec: &str) -> Option<String> {
    let rest = spec.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if meta.contains(";base64") {
        return None;
    }
    let decoded = percent_encoding::percent_decode_str(data).decode_utf8().ok()?;
    Some(decoded.into_owned())
}

/// Queue a `<script type="module">` body for deferred ES-module evaluation.
///
/// `load_html` already did this from Rust; the page driver -- the path the conformance runner and CDP
/// navigation use -- had no way to, so it ran modules through the classic-script path and every one
/// raised "Cannot use import statement outside a module", taking its whole file with it. Deferring is
/// browser semantics regardless: a module script is evaluated after the document parses.
///
/// `url` is the module's own URL — the resolved `src` for an external module script, the DOCUMENT's
/// url for an inline one — and it is what every stack frame in that module names, because V8 puts the
/// `ScriptOrigin` resource name in every frame and deno_core builds that origin from this string.
/// Empty means "no URL" (the `load_html` path, which has no document URL to speak of) and falls back
/// to `about:blank`, which is what a document with no URL is called -- against which no relative
/// import can resolve.
///
/// `import_base` is the base URL the body's relative `import`s resolve against, and is `Some` only for
/// an inline module, whose two URLs Chrome keeps apart under `<base href>` (see
/// [`PendingModule`]). An external module passes `None`: its own URL is its base.
#[op2(fast)]
pub(crate) fn op_queue_module(state: &mut OpState, #[string] source: String, #[string] url: String, #[string] import_base: String) {
    let import_base = (!import_base.is_empty()).then_some(import_base);
    state.borrow_mut::<PendingModules>().0.push(PendingModule { source, url, import_base, element: None, refused: false, inserted: false, delay_token: None, staged_slot: None, render_blocking: false });
}

/// The same queue, entered from Rust. The page-script driver is Rust and no longer
/// needs the op to be reachable by name from the page's realm.
pub(crate) fn queue_module(scope: &mut deno_core::v8::PinScope, source: String, url: String, import_base: Option<String>, render_blocking: bool) {
    let st = deno_core::JsRuntime::op_state_from(scope);
    st.borrow_mut().borrow_mut::<PendingModules>().0.push(PendingModule { source, url, import_base, element: None, refused: false, inserted: false, delay_token: None, staged_slot: None, render_blocking });
}

/// A parser module that blocks rendering until it has run is still queued ([`PendingModule::render_blocking`]).
pub(crate) fn render_blocking_modules(st: &deno_core::OpState) -> bool {
    st.try_borrow::<PendingModules>().is_some_and(|q| q.0.iter().any(|m| m.render_blocking))
}

/// Queue an EXTERNAL module script with its element. `body` is `None` when the element's response was
/// refused, which queues the failure in document order rather than dropping it -- the `error` event is
/// fired when the queue gets there, which is after the parse, as Chrome does.
pub(crate) fn queue_external_module(
    scope: &mut deno_core::v8::PinScope,
    body: Option<String>,
    url: String,
    element: deno_core::v8::Global<deno_core::v8::Value>,
    staged_slot: Option<usize>,
    render_blocking: bool,
) {
    let st = deno_core::JsRuntime::op_state_from(scope);
    // a staged slot not in yet: the body comes before the parse's end drains it ([`fill_staged_modules`])
    let refused = body.is_none() && staged_slot.is_none();
    st.borrow_mut().borrow_mut::<PendingModules>().0.push(PendingModule {
        source: body.unwrap_or_default(),
        url,
        import_base: None,
        element: Some(element),
        refused,
        // The PARSER found this one: it is deferred to the end of the parse, before DOMContentLoaded.
        inserted: false,
        // The runtime's own fetch pass already had these bytes; no load-delay token travels with them.
        delay_token: None,
        staged_slot,
        render_blocking,
    });
}

/// The streaming parse's end: every parser module queued before its response arrived takes it now --
/// `bodies[slot]`, `None` for a fetch that failed or a response the module flavour refuses.
pub(crate) fn fill_staged_modules(scope: &mut deno_core::v8::PinScope, bodies: &dyn Fn(usize) -> Option<Option<String>>) {
    let st = deno_core::JsRuntime::op_state_from(scope);
    let mut st = st.borrow_mut();
    let Some(q) = st.try_borrow_mut::<PendingModules>() else { return };
    for m in q.0.iter_mut() {
        // `None`: still not in -- the slot stays for the next call
        let Some(got) = m.staged_slot.and_then(&bodies) else { continue };
        m.staged_slot = None;
        match got {
            Some(b) => m.source = b,
            None => m.refused = true,
        }
    }
}

#[cfg(test)]
mod module_type_tests {
    use super::is_json_mime_type;

    /// The rows are lifted from WPT's own enumeration
    /// (`html/semantics/scripting-1/the-script-element/json-module/{valid,invalid}-content-type`),
    /// which is where the interesting cases live: every one of them was chosen because an
    /// implementation that reaches for `contains("json")` gets it wrong.
    #[test]
    fn json_mime_type_accepts_only_what_fetch_calls_json() {
        for ok in [
            "application/json",
            "text/json",
            "APPLICATION/JSON",
            "application/json;charset=utf-8",
            "  application/json  ",
            "application/vnd.api+json",
            "text/html+json",
            "image/svg+json",
            "application/json+json",
        ] {
            assert!(is_json_mime_type(ok), "{ok:?} should be a JSON MIME type");
        }
        for bad in [
            // `+json` must be a SUFFIX of the subtype, not an infix.
            "application/json+protobuf",
            "application/json+blah",
            // the subtype must BE `json`, not merely end in it.
            "text/x-json",
            "application/blahjson",
            // `application`/`text` only, for the bare `json` subtype.
            "image/json",
            // structurally not a MIME type at all.
            "text+json",
            "json+json",
            "text/json/json",
            "text/json/json+json",
            "/vnd.api+json",
            "",
            // the parameters are cut BEFORE the essence is judged, so this one is `text/html`.
            "text/html;+json",
            // neither half may hold a non-token byte: space, separator, control, non-ASCII.
            "applic ation/vnd.api+json",
            "application/vnd .api+json",
            "applic\tation/vnd.api+json",
            "applic\nation/vnd.api+json",
            "applic\"ation/vnd.api+json",
            "application/vnd(api+json",
            "application/vnd.api+json\"",
            "aplicación/vnd.api+json",
            "application/vnd.api🚀+json",
            // `+json` alone is not a subtype; something must precede the suffix.
            "application/+json",
        ] {
            assert!(!is_json_mime_type(bad), "{bad:?} should NOT be a JSON MIME type");
        }
    }
}
