//! THE ENGINE — assembling a V8 isolate that wears a persona, and the handful of services every
//! other kernel file reaches it through.
//!
//! Moved out of `lib.rs` VERBATIM. Nothing here was edited in the move.
//!
//! # What `assemble` is
//!
//! One `JsRuntime`, one context, one persona — built in an order where each step's position is stated
//! where it happens. Three things happen here that used to happen nowhere, or only in a banner: the
//! TRANSPORT is told which browser to be, the TIMEZONE and LOCALE come from the persona for every
//! entry point rather than only for `korvus/src/main.rs`, and the COHERENCE AUDIT runs and can refuse.
//!
//! Then the surface is installed: the block of table installs (each with its own recorded reason for
//! being where it is), and the passes that finish the surface.
//!
//! # The kernel ships no JavaScript (invariant I9)
//!
//! There WAS a `BOOTSTRAP` string here, evaluated between the table installs and the first page
//! script. It is deleted, and with it the last JavaScript in the window realm. Where each of its
//! pieces went, because every one of them is findable by name:
//!
//! | was in `BOOTSTRAP` | is now |
//! |---|---|
//! | `print` | the `print` row of the `Window` table (`core/html/window.rs::cb_print`) |
//! | the CSSOM block | `interface!` tables in `core/css/` (`install_window_cssom`) |
//! | `elementFromPoint`/`elementsFromPoint` | two `Document` rows (`core/dom/geometry_queries.rs`) |
//! | WebAssembly streaming | a `WebAssembly` namespace table (`modules/wasm_streaming.rs`) |
//! | the unhandled-rejection handler | `webapi::install_rejection_handler` |
//! | the bridge-symbol filter | nothing — the window realm has no bridge symbol to filter |
//! | stash-and-delete `Deno` | `webapi::hide_deno` |
//! | `KB.__recordUncaught` | nothing — `scheduler::run_timer` reports a throwing callback itself |
//! | the five timers + the pump | five `Window` rows over [`crate::scope_timers`] |
//! | the `[native code]` masking sweep | nothing — see below |
//!
//! THE SWEEP IS THE ONE THAT NEEDED A MEASUREMENT, not an argument. It replaced
//! `Function.prototype.toString` with a Proxy and marked every non-native function on the global and
//! nine host singletons, because a Web API implemented in JS reports its own SOURCE where a browser
//! reports `[native code]`. W4.f measured its roster at FORTY-FOUR functions and kept it on exactly
//! that evidence; every one of the 44 lived in `core/korvus-runtime/src/js/`, and that directory no
//! longer exists. Re-measured at W9.23 with the sweep removed:
//! `no_mask_live::every_function_on_the_global_surface_reports_native` stays green,
//! `nolies::zero_lies` and all three `automation_tells` tests stay green, `scripts/coverage.sh`
//! prints a byte-identical board (automation tells 0, non-native constructors 0, own-tells 0,
//! name-defects 0), and the whole-surface snapshot diff is the two clocks and nothing else. The
//! roster is empty, so the sweep masked nothing — and a patched `Function.prototype.toString` is
//! itself a first-order tell, so removing it is a gain in both directions.

use crate::{korvus_ext, singletons, HostPersona, KorvusModuleLoader, ModuleFetchState, PendingModules, PrintBuffer};
use deno_core::{JsRuntime, RuntimeOptions};
use spec::Persona;

/// A single-context JavaScript engine on real V8.
pub struct Engine {
    pub(crate) runtime: JsRuntime,
    /// Remote-object table for CDP (`Runtime.evaluate` objectId / `callFunctionOn`).
    pub(crate) objects: std::collections::HashMap<u32, deno_core::v8::Global<deno_core::v8::Value>>,
    pub(crate) next_object: u32,
    /// Behavior layer, now a swappable plugin: the input model (default = neuromotor)
    /// provided on the kernel Context, plus the virtual cursor position.
    pub(crate) behavior: std::sync::Arc<dyn behavior::BehaviorService>,
    pub(crate) cursor: spec::Point,
    /// What the module loader wears when it fetches an `import` over the network. Written by
    /// `load_page` (the only path that knows the document's URL and persona headers), read by
    /// [`KorvusModuleLoader`], which has no other way to reach either.
    pub(crate) module_fetch: std::rc::Rc<std::cell::RefCell<ModuleFetchState>>,
    /// RENDER #6: the `FetchedImages` generation this engine last painted at. `rendering_opportunity`
    /// repaints when the generation advances past it (an image finished loading), so a loaded image's
    /// `largest-contentful-paint` fires -- the sole compositor paint otherwise precedes the async load.
    /// Compositor-only (opt-in); seeded from the current generation so no spurious first-frame repaint.
    pub(crate) last_painted_img_gen: u64,
    /// SESSION 2b-repaint: the fed-frame generation (`plugin_host::fed_frame_generation`) this engine last
    /// painted at. `rendering_opportunity` forces ONE paint when it advances (a plugin fed a new camera frame),
    /// so a fed `<video>` is live on the default no-compositor path. Seeded from the current generation so a
    /// frame fed before this engine existed is not repainted; stays equal forever on any no-fed run (I9/J2).
    pub(crate) last_painted_fed_gen: u64,
}

/// Seed the motor persona deterministically from the device persona (FNV-1a over UA + platform), so a
/// session's motion is internally consistent but distinct across personas. Nonzero.
pub(crate) fn motor_seed(persona: &dyn Persona) -> u64 {
    let nav = persona.navigator();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in nav.user_agent.bytes().chain(nav.platform.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h | 1
}

/// Banner string used by the binary.
pub fn engine_name() -> &'static str {
    "korvus-engine (deno_core, G1.1)"
}

thread_local! {
    /// A test seam, never set in production. The gate runs with `KORVUS_HOST=ignore` so it can boot
    /// every persona bundle and grade its SERVED values, including one whose family this host cannot
    /// wear (a macOS persona on a Linux gate). That same ignore would stop `persona_gate` from proving
    /// the audit still REFUSES a persona for another machine, so that one test forces the audit on for
    /// the span of its own `assemble` via [`force_host_audit`], regardless of the ambient env.
    static FORCE_HOST_AUDIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with the host audit forced to BAIL on a `persona<->host` mismatch even under
/// `KORVUS_HOST=ignore`. Test-only ([`FORCE_HOST_AUDIT`]); production honours the env escape hatch.
pub fn force_host_audit<T>(f: impl FnOnce() -> T) -> T {
    FORCE_HOST_AUDIT.with(|c| c.set(true));
    let r = f();
    FORCE_HOST_AUDIT.with(|c| c.set(false));
    r
}

impl Engine {
    /// Boot a fresh isolate/context wearing `persona`. Every Web API member, `navigator` and the five
    /// timers included, is a table row installed by the one installer from Rust: the kernel evaluates
    /// no JavaScript of its own (invariant I9).
    pub fn new(persona: Box<dyn Persona>) -> Self {
        Self::assemble(persona).expect("the assembled persona must be coherent -- see the message")
    }

    /// Build an engine, refusing to build an incoherent one.
    ///
    /// THREE THINGS HAPPEN HERE THAT USED TO HAPPEN NOWHERE, OR ONLY IN A BANNER.
    ///
    /// 1. THE PERSONA'S WIRE IS CHECKED. It used to hold a `Chrome137` constant while the persona
    ///    derived its TLS id from its own UA, so nothing compared the wire to the identity. The
    ///    persona's declared profile is resolved here, and an id the pinned wreq-util cannot emit is
    ///    an error rather than a fallback.
    ///
    ///    IT IS A CHECK NOW AND NOT A SETTING. `transport::configure` used to claim the
    ///    preset for the whole process, so the SECOND engine in a binary -- which is what
    ///    `persona_surface` and `persona_diff` are -- was refused at assembly for naming a different
    ///    browser, while opening no socket at all. The wire travels on the request instead
    ///    (`FetchRequest::wire`, written from the persona beside its identity headers), so four
    ///    personas coexist and each one's requests carry its own ClientHello. What survives here is
    ///    the fail-closed half: a bundle naming a preset this build cannot emit still refuses to
    ///    start, rather than discovering it at the first fetch.
    ///
    /// 2. THE TIMEZONE COMES FROM THE PERSONA, FOR EVERY ENTRY POINT. `korvus/src/main.rs` set `TZ`
    ///    from the persona before booting, so `Intl.DateTimeFormat().resolvedOptions().timeZone`
    ///    matched the worn identity -- but only for that binary. The bench runner, the CDP server
    ///    embedded elsewhere, and every test inherited the HOST's zone, which is a UA/locale
    ///    mismatch and a bot tell. It belongs where the engine is built, so it cannot be forgotten
    ///    by a new caller.
    ///
    /// 3. THE COHERENCE AUDIT RUNS, AND IT CAN REFUSE. It ran in `banner()` -- the no-arguments
    ///    path -- so `korvus run|fetch|serve|probe` never audited anything, and its result was a
    ///    printed line either way. Now a contradiction the shipped bundle has not accounted for
    ///    stops the engine.
    ///
    /// The bundle's `known_incoherence` list is what makes that last one possible today: this tree
    /// really is mixed (see the manifest), so failing on every disagreement would refuse to start,
    /// and warning on them is the line nobody read. Listing them makes each countable, gives each a
    /// stated fix, and makes the next one fatal.
    pub fn assemble(persona: Box<dyn Persona>) -> anyhow::Result<Self> {
        Self::assemble_shared(std::sync::Arc::from(persona))
    }

    /// [`Engine::assemble`] over a persona that is already shared -- the one a document's SUCCESSOR
    /// wears (`navigation.rs::Engine::successor`). A cross-document navigation is a new
    /// realm on the same device, so the new isolate must wear the very `Arc` the old one did, not a
    /// re-resolved copy of it.
    pub fn assemble_shared(persona: std::sync::Arc<dyn Persona>) -> anyhow::Result<Self> {
        Self::assemble_with(persona, true)
    }

    /// [`Engine::assemble_shared`]. `snapshot`: whether the page realm may come from this build's snapshot of it
    /// (`crate::snapshot`); false for the engine built again after one did not restore.
    fn assemble_with(persona: std::sync::Arc<dyn Persona>, snapshot: bool) -> anyhow::Result<Self> {
        transport::check_wire(&persona.transport_profile())?;

        persona_env(persona.as_ref());

        // TWO GATES, because the two kinds of disagreement have different fixes.
        //
        // A persona that contradicts ITSELF is a build problem: the bundle says one thing in two
        // places. Nothing about the machine can excuse it, so it always refuses.
        //
        // A persona that contradicts the MACHINE UNDER IT is a deployment problem. The bundle is
        // internally fine; it just describes a computer that is not this one -- which is the normal
        // state of affairs the moment this repo is cloned somewhere else, and it stays true until
        // someone re-captures. That deserves to be loud, and it deserves a door: refusing outright
        // would mean a fresh clone cannot run a single page until it has produced its own capture.
        // Where an engine's construction goes, phase by phase (`KORVUS_NAV_TRACE`; inert otherwise).
        let t_host = std::time::Instant::now();
        let host = spec::coherence::HostFacts::measure();
        let (host_side, self_side): (Vec<_>, Vec<_>) =
            spec::coherence::unaccounted_on(persona.as_ref(), &host)
                .into_iter()
                .partition(|i| i.layers.starts_with("persona<->host"));

        let render = |v: &[spec::coherence::Incoherence]| {
            v.iter().map(|i| format!("  {} — {}", i.layers, i.detail)).collect::<Vec<_>>().join("\n")
        };

        if !self_side.is_empty() {
            anyhow::bail!(
                "the assembled persona is incoherent in {} way(s) the bundle does not account for:\n{}\n\n\
                 A page can see every one of these at once, and a device that disagrees with itself is a \
                 stronger bot signal than any single wrong value. Either fix the disagreement, or -- if it \
                 is genuinely unavoidable with this bundle -- add a `[[known_incoherence]]` to its manifest \
                 with a reason and a fix path, so it is counted rather than forgotten.",
                self_side.len(),
                render(&self_side)
            );
        }

        if !host_side.is_empty() {
            let detail = render(&host_side);
            // `ignore` is the whole vocabulary on purpose: one word, spelled out, that a person had
            // to type. Anything more configurable becomes something a script sets once and nobody
            // ever reads again, which is how the mismatch got here in the first place.
            if std::env::var("KORVUS_HOST").as_deref() == Ok("ignore")
                && !FORCE_HOST_AUDIT.with(|c| c.get())
            {
                // The browser endpoint says this once at startup and sets KORVUS_HOST_QUIET for its page
                // processes, so a stealth `serve` does not repeat it per page; a one-shot run keeps it (goal 1020
                // step 7, `korvus::apply_host`).
                if std::env::var("KORVUS_HOST_QUIET").as_deref() != Ok("1") {
                    eprintln!(
                        "korvus: wearing a persona that does not describe this machine ({} finding(s)), \
                         because KORVUS_HOST=ignore:\n{detail}",
                        host_side.len()
                    );
                }
            } else {
                anyhow::bail!(
                    "this persona describes a different machine than the one it is running on, in {} \
                     way(s):\n{}\n\n\
                     The bundle itself is coherent -- this is about the host under it. Three ways out:\n\
                       * capture this machine:  python3 plugins/korvus-persona/capture/host.py\n\
                         then promote it and select it with KORVUS_BUNDLE=<capture-id>\n\
                       * KORVUS_BUNDLE=<id> to wear a bundle that already matches this host\n\
                       * KORVUS_HOST=ignore to run anyway, accepting that a page measuring the host \
                     (worker timing, font availability, GPU strings) can see the difference.",
                    host_side.len(),
                    detail
                );
            }
        }

        crate::navtrace::mark("host_facts", t_host.elapsed().as_secs_f64() * 1000.0);
        let module_fetch = std::rc::Rc::new(std::cell::RefCell::new(ModuleFetchState::default()));
        // The persona's recorded device oracle, read BEFORE the persona is moved into the extension.
        // `js/generated_shells.js` read it from inside the page realm through `op_persona_oracle`;
        // that op and that file are deleted and the caller that holds the persona hands
        // it to `install_window_bootstrap` directly.
        let device_oracle_json = persona.device_oracle_json();
        // The realm's build is the single largest fixed cost of a navigation, and it
        // has three parts that move for different reasons. They are marked, not summed here.
        // The context snapshot (slice 3; `crate::snapshot`): this build's snapshot of the page realm for this
        // environment and persona, when there is one. The runtime is made from it, and the install below is replaced
        // by its restore. A miss starts the process that makes one.
        let snap = if snapshot { crate::snapshot::load(persona.as_ref()) } else { None };
        let again = persona.clone();
        let t_assemble = std::time::Instant::now();
        let mut extensions = vec![
                korvus_ext::init(persona),
                // ONE webapi extension, not two. `webapi_ext` was the window's ESM
                // layer -- five JS files whose whole content was a bootstrap sequence -- and it is
                // `install::live::install_window_bootstrap`, the first line of the block below.
                webapi::webapi_core_ext::init(), // native ops/objects (scope-agnostic)
            ];
        if let Some(s) = &snap {
            extensions.push(s.refs_ext());
        }
        let mut runtime = crate::new_runtime(RuntimeOptions {
            extensions,
            module_loader: Some(std::rc::Rc::new(KorvusModuleLoader { fetch: module_fetch.clone() })),
            startup_snapshot: snap.map(|s| s.blob),
            // the snapshot's op functions stay bound: deno keeps each op context's address in the external-reference
            // table, so they come back pointing at this runtime's; re-binding would replace `Deno.core.ops`' functions
            skip_op_registration: snap.is_some(),
            ..Default::default()
        });
        crate::navtrace::mark("v8_runtime_new", t_assemble.elapsed().as_secs_f64() * 1000.0);
        // the timer task source, asked between every two tasks whether a timer came due (`scheduler::wake_due_timers`)
        runtime.set_task_source_hook(crate::scheduler::wake_due_timers);
        // the loader's finished module fetches, for the page's resource timeline
        runtime.op_state().borrow_mut().put(module_fetch.borrow().timings.clone());
        // the page isolate's tasks, for long animation frames (`webapi::core::timing::loaf`)
        webapi::install_loaf_monitor(runtime.v8_isolate());
        // The table installs that run at context creation.
        //
        // "BEFORE / AFTER THE BOOTSTRAP" IN THE COMMENTS BELOW is a landmark that no longer exists as
        // code: the `BOOTSTRAP` string is deleted (this module's header says where its
        // pieces went). The SEAM it occupied is still there and still matters, because it is where
        // `webapi::hide_deno` deletes the `Deno` global -- so "before the bootstrap" reads as "while
        // `Deno` is still on the global", which is what every one of those positions was buying. Not
        // one install moved when the string went.
        //
        // the ten abort / style / blob / fetch tables used to HAVE to run here:
        // `CSSStyleDeclaration` is one of them, and `BOOTSTRAP`'s CSSOM block DECORATED
        // `CSSStyleDeclaration.prototype.cssText` so that a rule's declaration block
        // (`sheet.cssRules[i].style`, which has no element and so no `style` attribute to read)
        // answered its own text. Installed after the bootstrap, the table's plain row overwrote that
        // decorator and every at-rule block's `cssText` went back to "" -- the grader caught it as
        // `CSSPositionTryRule.style` and `CSSPositionTryDescriptors.top` losing `verified`.
        //
        // There is no decorator: the third kind of declaration block is a branch of the
        // `cssText` ROW now (`core/css/css_style_declaration.rs`), and the whole CSSOM is tables
        // (`install_window_cssom`, below). Nothing in this block is pinned by that any more. The order
        // is UNCHANGED regardless -- moving an install is a measurement, not a tidy-up.
        let t_realm = std::time::Instant::now();
        let mut restored: Result<(), String> = Ok(());
        {
            use deno_core::v8;
            let ctx = runtime.main_context();
            v8::scope_with_context!(scope, runtime.v8_isolate(), ctx);
            // THE WEBAPI EXTENSION'S ESM LAYER, IN RUST -- `js/generated_shells.js`'s
            // registry install, the crypto/XHR, media and element-ref lanes, the `SharedArrayBuffer`
            // gate and the device oracle, in that order.
            //
            // FIRST IN THIS BLOCK, and that is the identical position in the order rather than a new
            // one: those statements were the LAST bodies evaluated inside the `JsRuntime::new` two
            // statements above, and this block is the very next thing that runs. Nothing happens in
            // between. Everything below it kept the relative position it had.
            match snap {
                // the page realm deserialised: what its install left in Rust, back; then the per-run heap baseline
                Some(_) => restored = crate::snapshot::restore(scope).map(|()| webapi::mark_heap_baseline(scope)),
                None => install_window_realm(scope, &device_oracle_json),
            }
        }
        if let Err(why) = restored {
            crate::snapshot::failed(&why);
            drop(runtime);
            return Self::assemble_with(again, false);
        }
        if snap.is_none() {
            crate::snapshot::installed_fresh();
        }
        crate::navtrace::mark("window_realm", t_realm.elapsed().as_secs_f64() * 1000.0);
        // Deferred `<script type="module">` sources accumulate here; `run()` evaluates them (async).
        runtime.op_state().borrow_mut().put(PendingModules::default());
        // This browsing session's farbling seed -- one random u64 per Engine (a session), so
        // two serving connections farble independently while all of ONE session's origins derive from it
        // (Brave's model). The navigator getter reads it from OpState; workers inherit it (worker.rs). A
        // persona whose bundle does not farble (Chrome/Edge/Opera) ignores it and serves scalars static.
        // F6 (plugin ABI 3 wave 1): a plugin may force this process's pseudo-random stream reproducible
        // with `host->set_rng_seed` (so a persona's randomness replays). When one has, the engine mints
        // the session seed from the plugin's value instead of fresh entropy; workers inherit it (worker.rs),
        // and v8_features applies the same value as V8's `--random-seed` so Math.random matches too.
        let session_seed = {
            let seed = match plugin_host::rng_seed_override() {
                Some(forced) => forced,
                None => {
                    let mut b = [0u8; 8];
                    let _ = getrandom::getrandom(&mut b);
                    u64::from_le_bytes(b)
                }
            };
            spec::farble::SessionSeed(seed)
        };
        runtime.op_state().borrow_mut().put(session_seed);
        // THE SAME `Rc` THE LOADER HOLDS. A `ModuleLoader` is handed no `OpState` and no
        // scope, and the two places that install an import map have only a scope, so the store is
        // shared rather than passed: put the handle in `OpState` for the producers, and the loader
        // already owns its half through `ModuleFetchState`.
        runtime
            .op_state()
            .borrow_mut()
            .put(webapi::page::ImportMaps(module_fetch.borrow().import_maps.clone()));
        // `import(blobURL)`: the loader reads the page's blob registry through the same `Rc`.
        module_fetch.borrow_mut().blobs = webapi::blob_bytes_handle(&runtime.op_state().borrow());
        // The behavior model is a plugin: the extension's state closure registered a BehaviorPlugin on
        // the kernel Context (seeded from the persona); take its handle here. Swapping that plugin swaps
        // the input model without touching the engine's CDP-Input code below.
        let behavior = {
            let os = runtime.op_state();
            let st = os.borrow();
            st.borrow::<spec::kernel::Context>()
                .get::<behavior::BehaviorHandle>()
                .map(|h| h.0.clone())
        }
        .expect("behavior plugin provides a BehaviorHandle");
        // RENDER #6: seed the last-painted image generation from the current one so a reused/successor
        // engine does not repaint for images already fetched before it began.
        let last_painted_img_gen = {
            let os = runtime.op_state();
            let st = os.borrow();
            st.try_borrow::<spec::kernel::Context>()
                .and_then(|c| c.get::<spec::FetchedImages>())
                .map(|i| i.generation())
                .unwrap_or(0)
        };
        // SESSION 2b-repaint: seed the fed-frame cursor from the current generation, so a frame fed before this
        // engine existed is treated as already painted (mirrors last_painted_img_gen).
        let last_painted_fed_gen = plugin_host::fed_frame_generation();
        Ok(Self {
            runtime,
            objects: std::collections::HashMap::new(),
            next_object: 1,
            behavior,
            cursor: spec::Point::new(0.0, 0.0),
            module_fetch,
            last_painted_img_gen,
            last_painted_fed_gen,
        })
    }

    /// The version of V8 this engine actually runs, for a host that must report it (CDP's
    /// `Browser.getVersion` `jsVersion` / `/json/version`'s `V8-Version`, both of which answered the
    /// empty string). Not a persona fact: real Chrome reports its OWN V8 here, and so do we.
    pub fn js_version() -> &'static str {
        deno_core::v8::V8::get_version()
    }

    /// The identity this engine wears, for a host that has to answer FOR it — the CDP server's
    /// `Browser.getVersion` / `/json/version`, which used to answer a hardcoded `Korvus/0.0.0` (W9.5).
    ///
    /// Handing back the `Arc` rather than the values keeps the persona the single source: a caller
    /// asks it the question it needs answered instead of this file growing one accessor per field.
    pub fn persona(&self) -> std::sync::Arc<dyn Persona> {
        self.runtime.op_state().borrow().borrow::<HostPersona>().0.clone()
    }

    /// Execute a script synchronously. Microtasks/timers are NOT drained here — call
    /// [`Engine::run`] for that. Returning the completion value to Rust (needed for CDP
    /// `Runtime.evaluate`) is added in G1.8; G1.1 observes results via `print(...)`.
    pub fn eval(&mut self, name: &'static str, src: String) -> anyhow::Result<()> {
        self.runtime
            .execute_script(name, src)
            .map_err(|e| anyhow::anyhow!("execute_script failed: {e}"))?;
        Ok(())
    }

    /// RENDER: render the settled document to PNG bytes via the compositor paint walk
    /// (`korvus fetch --screenshot`). `None` until there is a page, a laid-out
    /// tree and a render backend. Runs inside a V8 scope (RC2): the style paint reads DOM/style/layout from
    /// op_state, and replaced content (`<img>`, and `<canvas>` next) is sourced through the scope.
    pub fn render_png(&mut self) -> Option<Vec<u8>> {
        self.with_scope(|scope| webapi::render_document_png(scope))
    }

    /// `Page.captureScreenshot`: the document painted, `clip` (`[x, y, width, height, scale]`, document CSS px) cut out
    /// of it, encoded as `format` -- see `webapi::render_document_shot`.
    pub fn render_shot(&mut self, format: spec::ImageFormat, quality: Option<f64>, clip: Option<[f64; 5]>, beyond: bool) -> Option<Vec<u8>> {
        self.with_scope(|scope| webapi::render_document_shot(scope, format, quality, clip, beyond))
    }

    /// The lifecycle the page document reached since it was reopened by `document.open()` and closed (`DOMContentLoaded`,
    /// `load`, each with when), for the driver to report as the frame's.
    pub fn take_reopened_lifecycle(&mut self) -> Vec<(&'static str, std::time::Instant)> {
        webapi::take_reopened_lifecycle(&mut self.runtime.op_state().borrow_mut())
    }

    /// `Page.printToPDF`: the document printed (see `webapi::render_document_pdf`).
    pub fn print_pdf(&mut self, opts: &webapi::PrintOptions) -> Result<Vec<u8>, String> {
        self.with_scope(|scope| webapi::render_document_pdf(scope, opts))
    }

    /// `Page.getLayoutMetrics`: `[client width, client height, scrollX, scrollY, content width, content height]`.
    pub fn layout_metrics(&mut self) -> [f64; 6] {
        self.with_scope(webapi::layout_metrics)
    }

    /// The process's time zone changed (`Emulation.setTimezoneOverride` set `TZ`): V8 drops its date caches and ICU
    /// reads the zone again, so `Date` and `Intl` answer in the new one.
    pub fn time_zone_changed(&mut self) {
        self.runtime.v8_isolate().date_time_configuration_change_notification(deno_core::v8::TimeZoneDetection::Redetect);
    }

    /// A viewport a DevTools client set or cleared (`Emulation.setDeviceMetricsOverride`): the page is owed a `resize`.
    pub fn viewport_changed(&mut self) {
        self.with_scope(webapi::window_resized);
    }

    /// A DevTools client turned touch emulation on (`Emulation.setTouchEmulationEnabled`): the current document's realm
    /// gets the `ontouch*` handlers, as every later one does as it starts.
    pub fn touch_emulation_changed(&mut self) {
        self.with_scope(webapi::devtools_log::install_touch_handlers);
    }

    /// The active render backend -- `Send + Sync`, so a thread of its own can encode the page's frames with it (a CDP
    /// screencast). `None` before there is one.
    pub fn render_backend(&mut self) -> Option<std::sync::Arc<dyn spec::RenderBackend>> {
        let os = self.runtime.op_state();
        let st = os.borrow();
        st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
    }

    /// Have the next rendering opportunity hand the frame tap the picture already on screen
    /// (`webapi::offer_frame_again`).
    pub fn offer_frame_again(&mut self) {
        self.with_scope(webapi::offer_frame_again);
    }

    /// The window's bounds, as a front end set them (`Browser.setWindowBounds`, D75): the window's inner and outer size
    /// and position follow, and with them the layout viewport, `@media` and the painted frame; a viewport that changed
    /// size owes the page a `resize`. `true` if it did.
    pub fn set_window_bounds(&mut self, b: webapi::WindowBounds) -> bool {
        let screen = self.persona().screen();
        let (before, after) = webapi::set_window_bounds(&screen, b);
        let moved = before != after;
        if moved {
            self.with_scope(webapi::window_resized);
        }
        moved
    }

    /// Run `f` inside the main context's V8 scope.
    ///
    /// This is what the page pipeline uses instead of `eval`. It is deliberately the
    /// SAME shape as `JsRuntime::execute_script`, which opens a context scope, compiles, runs and
    /// returns: neither drains microtasks, so moving a driver from a JS string to a Rust body does not
    /// move a single microtask checkpoint. (Verified against `deno_core` 0.410's
    /// `JsRealm::execute_script`, which is compile + run and nothing else.)
    pub(crate) fn with_scope<T>(&mut self, f: impl FnOnce(&mut deno_core::v8::PinScope) -> T) -> T {
        use deno_core::v8;
        let ctx = self.runtime.main_context();
        v8::scope_with_context!(scope, self.runtime.v8_isolate(), ctx);
        f(scope)
    }

    /// Has the page fired its `load` event for the current navigation?
    ///
    /// The HOST's own record ([`crate::LoadFired`] in `OpState`), written by the lifecycle step that
    /// fires the event and cleared when a navigation starts. `false` before the first navigation.
    ///
    /// THIS IS THE READ THAT REPLACED A PAGE EVAL. `pump_until_loaded` used to ask the page
    /// `(globalThis[Symbol.for('korvus.bridge')]||{}).__loadFired === true` -- one of the ten declared
    /// JS strings of the kernel, and the last reason `__loadFired` sat on the bridge where a page
    /// could read or forge it. It is `pub` because the readers that motivated the slot are outside
    /// this process: `korvus fetch --errors` prints it for `tools/corpus/run.py`, and two runtime
    /// tests assert on it directly. `Engine::eval_bool` was deleted with the probe -- it had no other
    /// caller.
    pub fn load_fired(&mut self) -> bool {
        let state = self.runtime.op_state();
        let state = state.borrow();
        state.try_borrow::<crate::LoadFired>().map(|f| f.0).unwrap_or(false)
    }

    /// Every request the transport made on this page's behalf since the current navigation began.
    ///
    /// THE ENGINE'S OWN RECORD, not the page's. `performance.getEntriesByType('resource')` answers a
    /// neighbouring question from inside the realm, and it is the wrong source for this one three
    /// times over: a page can redefine it, it omits the document's own navigation, and it says
    /// nothing about a request that never produced a response — which is precisely the row a
    /// resource-set diff against Chrome exists to find. The ledger is written by
    /// `transport::fetch`, the one entry every request passes through, and read here.
    ///
    /// The rows are in the order the requests STARTED, which is the order a CDP client expects
    /// `Network.requestWillBeSent` in.
    pub fn requests(&self) -> Vec<transport::RequestRecord> {
        transport::ledger().records()
    }

    /// Rows the ledger dropped at its cap during the current navigation (0 for any ordinary page).
    ///
    /// Reported beside [`Engine::requests`] rather than folded into it: a truncated resource set that
    /// does not say it was truncated reads exactly like an engine that fetched less than Chrome.
    pub fn requests_dropped(&self) -> u64 {
        transport::ledger().dropped()
    }

    /// `(page documents, frame documents)` this realm has laid out.
    ///
    /// A count and not a clock, for the efficiency box: a page with no frame -- or a frame nothing
    /// measures in -- must lay out no frame document at all, and a page must lay itself out once per
    /// change it measures after, not once per question. Like [`Engine::webgl_command_log`], exposed to
    /// Rust only; a page has no way to read it.
    pub fn layout_counts(&self) -> (u64, u64) {
        webapi::layout_counts(&self.runtime.op_state().borrow())
    }

    /// What this isolate's interface templates came from: how many it RECORDED (ran the
    /// install and translated it), how many it built from a blueprint another isolate of the process
    /// recorded -- a document's successor wears its predecessor's persona, so it builds the switched
    /// interfaces without running their installs -- and how many its realms instantiated. Rust only,
    /// like [`Engine::layout_counts`].
    pub fn template_counts(&self) -> webapi::TemplateCounts {
        webapi::template_counts(&self.runtime.op_state().borrow())
    }

    /// How many documents `korvus-page` has laid out.
    ///
    /// [`Engine::layout_counts`] counts what the runtime's geometry CACHE refreshed; this counts
    /// what the style layer actually ran, which is a bigger number because `getComputedStyle` lays
    /// the document out behind that cache when a `@container` rule's basis has moved. It is the
    /// counter "a second style read of an unchanged tree lays nothing out" has to be asserted
    /// against -- a count, not a clock, and Rust only, like its neighbour.
    pub fn document_layouts(&self) -> u64 {
        webapi::document_layouts(&self.runtime.op_state().borrow())
    }

    /// Every GL command this page has recorded, as `Debug` strings, newest context last.
    ///
    /// The RECORD half of the WebGL seam, exposed to Rust and only to Rust. A test has to be able to
    /// assert that `drawArrays(gl.TRIANGLES, 0, 3)` reached the command list as a `DrawArrays` op --
    /// and giving the page a JS-visible way to read the same thing would be a global no browser has,
    /// which is a worse tell than the one it would be proving the absence of.
    pub fn webgl_command_log(&mut self) -> Vec<String> {
        let state = self.runtime.op_state();
        let state = state.borrow();
        let mut out = Vec::new();
        let store = state.borrow::<webapi::WebGLStore>();
        let mut ids: Vec<&u32> = store.ctx.keys().collect();
        ids.sort();
        for id in ids {
            for op in &store.ctx[id].ops {
                out.push(format!("{op:?}"));
            }
        }
        out
    }

    /// Drain and return everything printed via `print(...)` so far.
    pub fn take_output(&mut self) -> Vec<String> {
        let state = self.runtime.op_state();
        let mut state = state.borrow_mut();
        std::mem::take(&mut state.borrow_mut::<PrintBuffer>().0)
    }
}

/// The zone the process started in, before a DevTools client's override: what clearing the override puts back.
static STARTING_TZ: std::sync::OnceLock<Option<std::ffi::OsString>> = std::sync::OnceLock::new();

/// `Emulation.setTimezoneOverride`: `TZ` for the page's process (`None` puts back the zone it started in). A realm built
/// after it reads it as it starts ([`persona_env`] respects a set `TZ`); one already running is told with
/// [`Engine::time_zone_changed`].
pub fn set_timezone_override(tz: Option<&str>) {
    let start = STARTING_TZ.get_or_init(|| std::env::var_os("TZ"));
    match (tz, start) {
        (Some(tz), _) => std::env::set_var("TZ", tz),
        (None, Some(s)) => std::env::set_var("TZ", s),
        (None, None) => std::env::remove_var("TZ"),
    }
}

/// `Emulation.setLocaleOverride`: ICU's default locale for the page's process -- `KORVUS_LOCALE`, which
/// [`persona_env`] puts first when a realm starts, and ICU's default now, for formatters made from here on. `None`
/// puts the persona's back.
pub fn set_locale_override(locale: Option<&str>) {
    match locale {
        Some(l) => {
            std::env::set_var("KORVUS_LOCALE", l);
            std::env::set_var("LC_ALL", format!("{l}.UTF-8"));
            deno_core::v8::icu::set_default_locale(l);
        }
        None => std::env::remove_var("KORVUS_LOCALE"),
    }
}

/// The process environment V8 and ICU read, from the persona, before either reads it: `TZ` (unless a deployment set
/// one) and `LC_ALL`. A function of its own so the process that makes the page realm's snapshot
/// (`crate::snapshot`) sets them exactly as an engine does.
pub(crate) fn persona_env(persona: &dyn Persona) {
    // Before V8/ICU reads either. Respect a pre-set value: a real deployment matches both to its
    // exit IP.
    if std::env::var_os("TZ").is_none() {
        let tz = persona.client_hints().timezone;
        if !tz.is_empty() {
            std::env::set_var("TZ", &tz);
        }
    }
    // ICU TAKES ITS DEFAULT LOCALE FROM THE ENVIRONMENT, AND NOBODY WAS SETTING IT. So
    // `navigator.language` came from the persona while
    // `Intl.DateTimeFormat().resolvedOptions().locale` came from the host's `LANG` -- measured on
    // this machine as `en-US` against `ko-KR`. Those two are among the most commonly compared
    // values in a fingerprint, and a page reading both saw a device whose UI language disagreed
    // with its own navigator. Setting it from the persona closes it.
    //
    // ...AND IT IS THE PERSONA'S `intl.locale`, NOT ITS FIRST LANGUAGE. Making
    // `navigator.languages[0]` the source closed the "ICU reads the host" half and opened a new
    // disagreement, because THE TWO FACTS ARE NOT THE SAME FACT. A default `Intl.DateTimeFormat()`
    // resolves to the browser's UI language; `navigator.language` is the Accept-Language head.
    // Measured 2026-09-18 against real Brave 151.1.93.138 over a loopback page: Brave answers
    // `"ko"`, korvus answered `"ko-KR"` -- one string, visible in one read, on a signal a bot sensor's
    // sensor touched 8 times in one page load. The four bundles in this tree record the two facts
    // SEPARATELY and disagree in BOTH directions -- brave/chrome/opera carry `intl.locale "ko"`
    // with `languages[0] "ko-KR"`, edge carries `intl.locale "ko-KR"` with `language "ko"` -- so
    // there is no derivation, only a capture, and `client_hints().intl_locale` is where it lives.
    // A bundle that carries no `intl.locale` still falls back to the first language rather than to
    // the host, which is the previous behaviour and strictly better than `LANG`.
    //
    // THE PERSONA WINS HERE, unlike TZ, and the asymmetry is deliberate. A deployment genuinely
    // matches its timezone to its exit IP, so a pre-set `TZ` is a decision worth respecting. A
    // pre-set `LANG` is not a decision at all -- it is whatever the developer's desktop happens
    // to be, and respecting it means every run on a non-en machine ships an incoherent browser
    // by default. This box is `ko_KR.UTF-8`, so the first version of this check did exactly
    // that: it skipped, and the mismatch stayed. `KORVUS_LOCALE` is the explicit override.
    let locale = std::env::var("KORVUS_LOCALE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| Some(persona.client_hints().intl_locale).filter(|l| !l.is_empty()))
        .or_else(|| persona.navigator().languages.first().filter(|l| !l.is_empty()).cloned());
    if let Some(locale) = locale {
        std::env::set_var("LC_ALL", format!("{locale}.UTF-8"));
    }
}

/// The window realm's whole install, at engine start. A frame realm runs none of it: it is built from the
/// templates this install records (`browsing_context::install_lazy_frame` in `webapi`).
pub(crate) fn install_window_realm(scope: &mut deno_core::v8::PinScope, oracle: &str) {
    let t_install = std::time::Instant::now();
    let global = scope.get_current_context().global(scope);
            webapi::install::live::install_window_bootstrap(scope, oracle);
            webapi::install_codegen_hook(scope);
            // Everything `js/dom/09-postshells.js` did after the shell pass, ending
            // with the three surface-finishing passes (WebIDL arity guards, the recorded enumeration
            // order, the constant seal).
            //
            // FIRST IN THIS BLOCK, because that file is the webapi extension's ESM ENTRY POINT: its
            // body was the last thing evaluated inside the `JsRuntime::new` two statements above, and
            // this block is the very next thing that runs. Nothing happens in between, so this line
            // occupies the identical position in the order -- after `generated_reflect.js`'s
            // backfill, `dom.js`'s phases, `generated_shells.js`'s registry install and canvas
            // oracle, and `dom/09k-svg.js`'s borrow pass, and before every table install below.
            // Moving it later would sort and guard prototypes the lanes below have not built yet.
            // The foreign-content shared-mixin BORROW table and the three webref/Blink
            // arity disagreements -- the last two passes of `dom/09k-svg.js`, which is deleted with
            // this line. FIRST IN THIS BLOCK, because that file was the LAST import of the webapi
            // extension's ESM entry point: its body was the final thing evaluated inside the
            // `JsRuntime::new` two statements above, and this block is the very next thing that runs.
            // The slot decides which donors exist (`HTMLScriptElement` is installed further down this
            // block, so the `SVGScriptElement` row still borrows nothing), so moving it is a surface
            // change, not a tidy-up. The element FACTORY the same file registered is `korvus_dom::tags`
            // through `install::dom_chain::fill_dom_protos` now.
            // `window`/`self`/`top`/`parent`/`frames` and the `navigator`/`screen`/
            // `location` singletons, replacing the top of `host.js`.
            //
            // FIRST IN THIS BLOCK, because it is the earliest Rust that runs after the ESM layer and
            // these eight were defined at the START of that layer -- so this is the smallest move of
            // their definition point that Rust can make. Nothing in the ESM layer reads any of them
            // (grepped across `core/korvus-webapi/src/js/`), and the `BOOTSTRAP` masking sweep that
            // used to read `navigator`/`screen`/`location` after this whole block is deleted.
            //
            // W6.2's finding said these lines could never move, because they name deno_core
            // `objects =` classes and those are "reachable only from ESM". That is true of
            // `ext:core/ops` and false of the classes: they are realised onto `Deno.core.ops`, which
            // `webapi::binding::hidden::native_class` reads. See `core/html/window_globals.rs` for
            // the measured reason the three interface OBJECTS (`Screen`, `TextEncoder`,
            // `TextDecoder`) could NOT come with them -- a sequencing blocker, not a reachability one.
            webapi::install::live::install_window_globals(scope, global);
            // (`install_window_foreign_elements` stood here: its borrow table and its three arity guards are
            // registry rows now -- `tools/sync/gen_shells.py`.)
            // The `CSS` NAMESPACE object, replacing `dom/06-misc.js`. BEFORE
            // postshells, and that is a dependency rather than a preference: postshells ends by
            // calling `install_css_highlights`, which reads `globalThis.CSS` and defines
            // `highlights` on it. Installed after, that read finds nothing and `CSS.highlights`
            // silently disappears. The deleted JS ran in the ESM layer, before this whole block, so
            // this is the same relative order.
            webapi::install::live::install_window_css(scope, global);
            webapi::install::live::install_window_postshells(scope, global);
            // The EVENT FAMILY -- `Event` and the fifteen interfaces that inherit from it,
            // plus `Touch` and `MessageChannel` -- from their `interface!` tables with native
            // constructors, replacing the eighteen JS classes that used to be built during host.js's
            // ESM evaluation.
            //
            // SECOND IN THIS BLOCK, and the position is measured. `Event` existed from the ESM layer,
            // which is before everything, and four lanes BELOW splice a child onto it: L5's
            // AnimationEvent / TransitionEvent / AnimationPlaybackEvent and L1's
            // SecurityPolicyViolationEvent. Installed after the bootstrap -- where
            // `events::install_event_model` runs -- all four lost their chain and
            // `new SecurityPolicyViolationEvent("x").type` went undefined. It is after postshells
            // because that lane is the webapi extension's ESM epilogue; the nineteen names leave the
            // registry with their tables, so the two cannot interact. (They left generated_order.rs
            // too, until that second table was deleted.)
            webapi::install::live::install_window_event_family_now(scope, global);
            // The nine Web Animations interfaces (Animation, AnimationEffect,
            // KeyframeEffect, AnimationTimeline, DocumentTimeline, ScrollTimeline, AnimationEvent,
            // TransitionEvent, AnimationPlaybackEvent), replacing `dom/09j-animation.js`. FIRST in
            // this block, and specifically BEFORE L12, because `install_window_abort_style` ends with
            // `rechain_orphans`: `ViewTimeline` (parent ScrollTimeline), `CSSAnimation` and
            // `CSSTransition` (parent Animation) are still registry interfaces whose chain pass ran
            // during the ESM layer and could not find a parent that did not exist yet, and that
            // sweep is the repair -- it can only reach a parent already on the global when it runs.
            webapi::install::live::install_window_animations(scope, global);
            // `FeaturePolicy`, which `document.featurePolicy` hands back.
            // No ordering -- its parent is `Object.prototype`, nothing inherits from it, and the
            // seat below places the global at the bundle's own position.
            webapi::install::live::install_window_feature_policy(scope, global);
            webapi::install::live::install_window_abort_style(scope, global);
            // The CSSOM -- `StyleSheet`, `CSSStyleSheet`, `MediaList`, `CSSRule`,
            // `CSSStyleRule`, `CSSPositionTryRule`, `CSSCounterStyleRule` and
            // `CSSPositionTryDescriptors` from their `interface!` tables, replacing the ~300-line
            // CSSOM block of the deleted `BOOTSTRAP` string.
            //
            // AFTER `install_window_abort_style`, and that is a dependency rather than a preference:
            // that lane installs `CSSStyleDeclaration`, and `CSSPositionTryDescriptors` inherits from
            // it -- `create_interface` splices the child onto a parent that has to exist by then.
            // Before the bootstrap script only because this whole block is; the CSSOM no longer cares
            // which side of it runs first, because nothing in the script touches a CSSOM member.
            webapi::install::live::install_window_cssom(scope, global);
            // `WebAssembly.instantiateStreaming` / `compileStreaming` as two rows of a
            // `WebAssembly` namespace table, replacing two `async function` expressions in the
            // deleted `BOOTSTRAP` string and the two `KB.__markNative` calls that masked them. They
            // exist because deno_core leaves V8's WASM streaming host callback UNSET, and V8 invoking
            // it panics the process -- read `modules/wasm_streaming.rs` for the whole reason.
            //
            // No ordering: `WebAssembly` is a V8 intrinsic that exists from context creation, and
            // nothing else in the tree touches either member (grepped).
            //
            // The function no longer has `window` in its name, because `worker.rs` calls
            // THE SAME ONE. It never read a `global` argument -- it finds `WebAssembly` off whatever
            // context it runs in -- so the worker realm needed a call site, not a copy.
            webapi::install::live::install_wasm_streaming_rows(scope);
            // The native `unhandledrejection` handler, replacing the JS closure the
            // `BOOTSTRAP` string used to hand to `Deno.core.setUnhandledPromiseRejectionHandler`.
            //
            // HERE because this is the last block in which `Deno` is still on the global --
            // `webapi::hide_deno`, the statement after it, deletes it -- and because the handler
            // itself only ever runs at event-loop time, so installing it this early costs nothing.
            webapi::install_rejection_handler(scope);
            // NOTHING FILTERS `Symbol(korvus.bridge)` OUT OF THE GLOBAL HERE ANY MORE,
            // because the window realm no longer has one to filter. `webapi::hide_bridge_symbol`
            // replaced `Object.getOwnPropertySymbols` and `Reflect.ownKeys` with functions that
            // dropped that one symbol from the global's answer; both intrinsics are V8's own again,
            // which is strictly less to detect than two honest replacements.
            // `Worker` -- its native constructor and its four receive pumps, replacing
            // `host.js`'s `class Worker extends EventTarget`.
            //
            // IN THIS BLOCK, and not after the bootstrap, for one measured reason: the install
            // captures `Deno.core.unrefOpPromise` (a parked receive must not by itself keep the page
            // event loop alive), and `webapi::hide_deno` -- the statement after this block -- deletes
            // the `Deno` global. `host.js` captured the same function at ESM-evaluation time with the same
            // note; this is the next thing that runs after the ESM layer, so it is the same moment.
            webapi::install::live::install_window_workers(scope, global);
            // `ServiceWorker`, `ServiceWorkerRegistration` and `ServiceWorkerContainer`
            // from their `interface!` tables, replacing the 180-line ServiceWorker block of
            // `host.js` -- which REPLACED thirteen of these prototypes' D2 stub members at runtime
            // with `op_nativize`-masked JS closures, on prototypes the registry install had already
            // built (two writers per slot, and thirteen native masks).
            //
            // IMMEDIATELY AFTER `install_window_workers`, and that is a dependency: the container's
            // `register()` calls `shared_worker::start_isolate_pumps`, which arms the same pumps
            // `SharedWorker` does and reads the `Deno.core.unrefOpPromise` that lane's
            // `capture_unref` parks -- `webapi::hide_deno`, after this block, deletes `Deno`.
            //
            // The three names left the generated registry with the tables, so this is their only
            // source, and `navigator.serviceWorker` -- a LAZY row, so readable from the page's first
            // statement -- mints its container on these prototypes.
            webapi::install::live::install_window_service_workers(scope, global);
            // The eleven small element interfaces (HTMLInputElement, HTMLButtonElement,
            // HTMLObject/EmbedElement, HTMLLinkElement, HTMLIFrame/FrameElement, HTMLTextAreaElement,
            // HTMLAnchorElement, SpeechSynthesisUtterance, SecurityPolicyViolationEvent), replacing
            // nine `dom/*.js` files. Here for the same reason L12 is: BOOTSTRAP's CSSOM block owns
            // `HTMLLinkElement.prototype.sheet` (a real CSSStyleSheet over korvus-page's parser) and
            // this lane has no Rust body for it, so the table carries an honest `stub` row that puts
            // `sheet` in the capture's POSITION and the bootstrap's later define -- which does not
            // move an existing key -- supplies the body. Installed after the bootstrap, the stub
            // would win and `link.sheet` would go undefined. W4.f retires the decorator.
            webapi::install::live::install_window_small_elements(scope, global);
            // The six streams interfaces (ReadableStream,
            // ReadableStreamDefaultReader, ReadableStreamDefaultController, WritableStream,
            // WritableStreamDefaultWriter, TransformStream), replacing 95 lines of
            // `dom/08g-observers.js`. Here rather than after the bootstrap because that is where the
            // deleted JS block ran (during `dom.js`, before this script): `host.js`'s `enumerateProto`
            // sweep READS `globalThis.ReadableStream`, so the interface object has to exist by then.
            webapi::install::live::install_window_streams(scope, global);
            // The six observer interfaces (PerformanceObserver +
            // PerformanceObserverEntryList, IntersectionObserver + IntersectionObserverEntry,
            // ResizeObserver + ResizeObserverEntry), replacing the rest of
            // `dom/08g-observers.js`'s observer blocks and the two class-template assignments to
            // the global in `dom/00-bootstrap.js`. Here for the same reason L11b is: that is where the deleted JS
            // ran (during `dom.js`, before this script), and `host.js`'s `enumerateProto` sweep
            // READS `globalThis.PerformanceObserver`.
            webapi::install::live::install_window_observers(scope, global);
            // An earlier change installed `CustomEvent` and `PointerEvent` HERE, taking the prototype of
            // `host.js`'s class in place. W4.b deleted those classes: both interfaces are now rows of
            // the event family's own tables with native constructors, installed as the second
            // statement of the pre-bootstrap block. Two lanes tabled the same two interfaces from
            // opposite ends and the fuller one won -- see `install_window_event_family_now`.
            // `URLSearchParams` (and `URL`), replacing the last two blocks of
            // `dom/08g-observers.js`, the class-template assignment in `dom/00-bootstrap.js` and
            // `host.js`'s URL block. Here for the same reason L11b/L11c are: that is where the
            // deleted JS ran (during `dom.js`, before this script), and `host.js`'s `enumerateProto`
            // sweep READS the interface objects off the global.
            webapi::install::live::install_window_url(scope, global);
            // Web Storage -- the `Storage` interface object and the `localStorage` /
            // `sessionStorage` area objects, replacing `dom/01-interning.js`. Here rather than after
            // the bootstrap because that is where the deleted JS ran (the SECOND phase of `dom.js`,
            // before this script), and because `BOOTSTRAP` and `host.js` both read the global by
            // name: a page's `localStorage` must be answerable from the first line of page script,
            // and `host.js`'s `enumerateProto` sweep reads `globalThis.Storage`.
            webapi::install::live::install_window_storage(scope, global);
            // The forms family (HTMLFormElement, HTMLOutputElement,
            // HTMLFieldSetElement, HTMLMeterElement), replacing `dom/08w-form-element.js`,
            // `dom/08y-output-fieldset.js` and `dom/08m-meter-element.js`. Here rather than after the
            // bootstrap because that is where the deleted JS ran (during `dom.js`, before this
            // script) and because `install::dom_chain::fill_dom_protos` -- which runs after the
            // bootstrap -- must point the `form`/`output`/`fieldset`/`meter` tags at THESE
            // prototypes. Nothing left in the tree defines a member of the four (grepped).
            webapi::install::live::install_window_forms(scope, global);
            // `HTMLScriptElement`, replacing `dom/08t-script-element.js`. Here rather
            // than after the bootstrap for the two reasons every element lane has: that is where the
            // deleted JS ran (during `dom.js`, before this script), and `install::dom_chain::
            // fill_dom_protos` -- which runs after the bootstrap -- must point the `script` tag at
            // THIS prototype. The interface left the registry with its table, so this line is also
            // the only thing that puts it on the global.
            webapi::install::live::install_window_script_element(scope, global);
            // `HTMLBodyElement`, replacing `dom/08q-body-element.js`. Here for the
            // same two reasons the script element is: that is where the deleted JS ran, and
            // `fill_dom_protos` must point the `body` tag at THIS prototype. It also binds the
            // native `__bodyHandlerAttr` the `ACTIVATE_BODY_HANDLERS` driver below calls, so that
            // must happen before the parse finishes -- which this block, at context creation, is.
            webapi::install::live::install_window_body_element(scope, global);
            // The five element interfaces whose members neither the registry's
            // `[Reflect]` kit rows nor an element lane defined -- `<select>`, `<datalist>`,
            // `<progress>`, `<style>` and `<slot>` -- replacing the last mounts of
            // `dom/08b-reflected.js`. Here rather than after the bootstrap for the reason
            // `install_window_small_elements` is: `BOOTSTRAP`'s CSSOM block decorates
            // `HTMLStyleElement.prototype.sheet`, and a define does not move an existing key, so the
            // table's honest stub row has to be in place FIRST for the decorator to wrap it.
            webapi::install::live::install_window_per_tag_elements(scope, global);
            // The WebRTC value objects (RTCSessionDescription, RTCIceCandidate),
            // replacing two blocks of `dom/09k-media-rtc.js`. Here rather than after the bootstrap
            // because that is where the deleted JS ran (during `dom.js`, before this script) and
            // because `host.js`'s `enumerateProto` sweep READS the interface objects off the global.
            // Nothing left in the tree defines a member of either (grepped).
            webapi::install::live::install_window_media_rtc(scope, global);
            // `Window` from its `interface!` table -- the two constants on
            // `Window.prototype`, the other 155 rows on `globalThis` (WebIDL puts a `[Global]`
            // interface's members on the global object), and the global's re-parent onto that
            // prototype. Replaces `dom/09f-window.js` AND `generated_shells.js`'s Window special case,
            // which can no longer run during the ESM layer because the interface object does not exist
            // until this does.
            //
            // HERE, in the pre-bootstrap block, and not after it -- the position `09f-window.js` had
            // relative to `BOOTSTRAP`. It used to matter for exactly one row: `print`. BOOTSTRAP
            // assigned the engine's own `globalThis.print` (the op_kprint debug sink every test and
            // every `korvus fetch` reads) over the table's `print` no-op, so installing this table
            // AFTER the bootstrap silenced the engine while every test still passed green.
            //
            // That is closed: the sink IS the table's `print` row now (`window.rs::cb_print`),
            // nothing assigns over it, and no order buys anything for it. The position is UNCHANGED
            // regardless -- moving an install is a measurement, and this unit did not measure this one.
            webapi::install::live::install_window_window(scope, global);
            // `window.chrome` -- the Chrome-proprietary `{loadTimes, csi, app}` object,
            // replacing the 37-line IIFE at the top of `host.js`. Five of the forty-four names in
            // `tests/no_mask_live.rs`'s roster go with it: these functions report `[native code]` by
            // BEING native, so the masking sweep below no longer has anything to mark on `chrome`.
            //
            // LAST IN THIS BLOCK, and the position buys one thing: `chrome.app.installState` delivers
            // its callback through `globalThis.setTimeout`, which is a `Window` table row
            // (`install_window_window`, the line above). It is only read when a page calls
            // `installState`, so the dependency is not strictly ordered -- but a sibling install that
            // needed it at INSTALL time would be, and keeping the two adjacent is what makes that
            // visible. Nothing else in the block reads `window.chrome`.
            //
            // The descriptor is `configurable:false` (measured), so this is also the last chance to
            // define it: anything later would need to replace a non-configurable key and could not.
            webapi::install::live::install_window_chrome(scope, global);
        // THE `BOOTSTRAP` SCRIPT RAN HERE, and it is deleted -- see this module's header
        // for where each of its pieces went. The kernel evaluates no JavaScript of its own now.
        //
        // Stash `Deno.core.ops` and `evalContext`, then delete `Deno` and `__bootstrap`
        // from the page's global. HERE, at the seam the bootstrap script used to occupy: everything
        // the block above needs from `Deno` has by then been taken, and nothing between here and the
        // first page script looks for it.
            webapi::hide_deno(scope);
        // Phase 3 one-table cutover, live-in-parallel: reconcile the
        // natively-realised DOMRect family to its `interface!` table -- supply the Symbol.toStringTag,
        // WebIDL fn `.length`s, and constructor-inheritance chain op2 leaves out, and verify the
        // native surface still matches the table. Additive and idempotent (it re-installs no bodies
        // and removes nothing), so it is safe alongside the generated shell. Drift is a table/capture
        // mismatch worth surfacing; it is never fatal to a page load.
            // (the `guard_native_class_ctors` sweep stood here: every global still holding a raw
            // `deno_core` class template got the interface object a page should see. Templates made every
            // class an own class built from its table, so no global holds one and the sweep is deleted.)
            // (The geometry families' four `reconcile_*` verifications stood here: now they are
            // OWN classes, built from their tables at the bootstrap's class slot -- `install::live::geometry`.)
            // `Headers` and `FormData` -- the two Fetch cppgc value classes, from
            // ONE table each, in this realm and in the worker (`install::live::fetch_values`).
            for d in webapi::install::live::install_fetch_values(scope) {
                eprintln!("[korvus] one-table drift: {}.{}: {}", d.interface, d.member, d.detail);
            }
            // The window `Navigator`: its `interface!` table is the SOLE definition of every one of
            // the 66 members, shape and body (`korvus-webapi`'s core/navigator/). Runs
            // AFTER the whole ESM layer, because the shape-only device singletons `navigator.usb` and
            // friends are minted on the prototypes `generated_shells.js` creates -- and every
            // object-valued row builds on FIRST READ, behind the per-document surface-finishing
            // passes, exactly where the deleted `nav-glue.js` closures built theirs.
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_navigator(scope, global);
            }
            // Phase 3 Track C, live-in-parallel: build `HTMLImageElement` from its `interface!` table
            // (native CSSOM/loader ops + the reflect DSL over content attributes) spliced onto the real
            // HTMLElement.prototype, re-register the `img` tag onto it, and install the legacy `Image`
            // factory sharing that prototype. Runs AFTER the ESM layer (07-spine/08b/08k/09i), so it
            // OVERWRITES that glue's HTMLImageElement + Image -- the table is now the live authority.
            // The old glue stays a proven-identical fallback until the flip commit deletes it;
            // `tests/html_image_live.rs` proves byte- and value-identity.
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_html_image_element(scope, global);
            }
            // The six `<table>` interfaces from their `interface!` tables, replacing
            // `dom/08s-table-element.js` and `dom/08x-table-parts.js`. BEFORE the tag fill below, so
            // `table`/`tr`/`td`/`th`/`thead`/`tbody`/`tfoot`/`col`/`colgroup`/`caption` map onto these
            // prototypes through the one `korvus_dom::tags` pass instead of ten hand writes. The six
            // left both generated registries with the tables, so nothing else defines them.
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_tables(scope, global);
            }
            // `CustomElementRegistry` (+ the `customElements` singleton),
            // `MutationObserver` / `MutationRecord` and `HTMLDialogElement` from their `interface!`
            // tables, replacing four hand-built blocks of `dom/08g-observers.js`. BEFORE the tag fill
            // below, for the same reason the L2 family is: `dialog` must map onto the table's
            // prototype through the one `korvus_dom::tags` pass.
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_mutation_custom_elements(scope, global);
            }
            // the arity-guard repair pass used to run here. It existed only because
            // `dom/09k-media-rtc.js`'s `meth()` helper re-defined five of the media family's members
            // as bare native functions, dropping the guard `installer::method_row` had put on. W5.1
            // lane L4 moved those five bodies into `core/html/text_tracks.rs` and the table rows now
            // name them, so nothing re-defines an L3 member after `install_window_media` and there is no
            // guard left to repair -- measured by deleting the pass and running the gate, including
            // `tests/arity_guards.rs` and `tests/media_live.rs`'s own guard case.
            // `Range` (34 rows) and `Selection` (29) from their `interface!` tables,
            // replacing `dom/08i-range.js` and the 49 `op_range_*` / `op_sel_*` factory ops it was
            // the only caller of. AFTER the whole ESM layer AND after the bootstrap seam, so the tables are
            // the LAST writer of both prototypes and the SOLE one: the bootstrap's "Range geometry"
            // block, which used to assign `op_helper_method` bodies over `getBoundingClientRect` and
            // `getClientRects` (two `KB.__` slots, two native masks), is deleted in the same commit
            // -- those two bodies are `core/dom/range.rs`'s `range_box_element` now. Nothing else in
            // the tree defines a member of either interface (every `.js` in both crates and both
            // `lib.rs`/`worker.rs` string literals grepped for `Range` and `Selection`; the only
            // other hits are `host.js`'s two CLASSES, which stay the factories until W4.b).
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_ranges(scope, global);
            }
            // The wrapper-retype maps (nodeType -> prototype, HTML tag -> prototype, SVG
            // local name -> prototype) from ONE data source, `korvus_dom::tags`, now that the spine
            // tables and the registry have created every prototype they name. An interface the table
            // names that the realm does not expose is a boot defect, said out loud.
            // The page's alone: the maps are the PAGE realm's prototypes, and a frame's wrappers are
            // translated into the frame's realm from them (`protos::proto_in_realm`).
            let fill = webapi::install::dom_chain::fill_dom_protos(scope);
            for m in &fill.missing {
                eprintln!("[korvus] dom tag table names an interface the realm does not expose: {m}");
            }
            // The ten platform LIST interfaces from their `interface!` tables. Every list
            // a page is handed -- HTMLCollection, NodeList, DOMTokenList, NamedNodeMap and the rest --
            // is minted from these prototypes, so they must exist before any page script runs. The
            // interfaces left the generated registry when the tables were added, so nothing else on the
            // surface defines them.
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_collections(scope, global);
            }
            // The twelve IndexedDB interfaces from their `interface!` tables, plus the
            // `indexedDB` global. AFTER the collections, because `objectStoreNames` / `indexNames`
            // answer a `DOMStringList`, and after the event model, because IDBRequest / IDBTransaction /
            // IDBDatabase chain to `EventTarget` and IDBVersionChangeEvent to `Event`. The interfaces
            // left both generated registries with the tables, so nothing else defines them.
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_indexeddb(scope, global);
            }
            // DataTransfer / DataTransferItemList / DataTransferItem / FileList from
            // their `interface!` tables, replacing `dom/08d-datatransfer.js` and the four raw cppgc
            // class templates `dom/00-bootstrap.js` used to put on the global. AFTER the ESM layer, so
            // the table's interface object is the one a page sees; the interfaces left both generated
            // registries with the tables, so nothing else defines them.
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_datatransfer(scope, global);
            }
            // PluginArray / Plugin / MimeTypeArray / MimeType from their `interface!`
            // tables -- replacing the `navigator.plugins` block of `dom/09i-singletons.js`. The four
            // interfaces left the generated registry with the tables, so nothing else defines a member
            // of any of them.
            //
            // It used to matter that this ran AFTER the navigator install, because it ended by writing
            // the two `navigator` accessors over the Navigator table's own rows. W4.a deleted that
            // re-point: the Navigator rows name `plugins::navigator_plugin_family` and build the family
            // on first read, which is after this call wherever it sits.
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_plugins(scope, global);
            }
            // `History` from its `interface!` table, `location`'s `[LegacyUnforgeable]`
            // reshape and `performance`'s re-home -- the last of `dom/09i-singletons.js`. AFTER the
            // whole ESM layer for the reason that module's header gave: `generated_shells.js` rebuilds
            // every interface's prototype chain when it runs, so anything done before it is silently
            // undone, and `host.js` / `shared-globals.js` build these objects long before any
            // interface object exists.
            {
                let global = scope.get_current_context().global(scope);
                singletons::install_window_singletons(scope, global);
            }
            // The performance TIMELINE -- `Performance`, `PerformanceEntry`,
            // `PerformanceMark`, `PerformanceMeasure`, `PerformanceTiming`, `PerformanceNavigation`
            // and `EventCounts` from their `interface!` tables, and the `performance` singleton.
            //
            // EXACTLY THE SLOT `rehome_performance` OCCUPIED, one line above, and for the same
            // reason: `generated_shells.js` rebuilds every interface's prototype chain when it runs,
            // so anything done before it is silently undone. Nothing after this point writes a member
            // of any of the seven (grepped: `host.js`'s block is deleted, `BOOTSTRAP` names none of
            // them, and `install::live::rechain_orphans` only repairs an EMPTY child prototype).
            //
            // AFTER `install_window_singletons` rather than before it, because that function's
            // `strip_own` + re-prototype passes walk the global's own keys and the timeline is one of
            // them; installing first would hand it to a pass that has nothing left to do for it.
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_performance(scope, global);
            }
            // `SpeechSynthesis` from its `interface!` table plus the `speechSynthesis`
            // singleton, and the legacy `Audio` / `Option` element factories -- two more blocks of
            // `dom/09i-singletons.js`. AFTER the ESM layer, because both OVERWRITE a generated shell:
            // `SpeechSynthesis`'s prototype does not exist until `generated_shells.js` has run, and a
            // factory's shell has a non-writable `.prototype`, so the whole function is replaced.
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_speech_synthesis(scope, global);
                webapi::install::live::install_window_legacy_factories(scope, global);
            }
            // The window's remaining own globals -- the 43 names the worn capture records
            // that nothing else creates. AFTER the registry install (each one stands on an interface
            // prototype the registry published) and before the seat, which places them.
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_extras(scope, global);
            }
            // Splice the Window named-properties object into the prototype chain,
            // replacing `dom/09h-window-named.js`. LAST in this block, and after the whole ESM layer,
            // for the reason that file existed at all: `generated_shells.js` rebuilds every
            // interface's prototype chain when it runs, so a splice before it is silently undone.
            // Nothing after this point touches `Window.prototype`'s [[Prototype]] (grepped: the only
            // other writer is `install::live::rechain_orphans`, which skips this object by name).
            {
                let global = scope.get_current_context().global(scope);
                webapi::install::live::install_window_named_properties(scope, global);
            }
            // SEAL THE INTERFACE PROTOTYPES. Every mint site in `core/**` used to find
            // its prototype with `globalThis[Iface].prototype`, a property the page can write; a
            // page that reassigned `MediaList` got the engine's own `sheet.media` minted onto ITS
            // object. `webapi::seal_interface_protos` snapshots the finished global into a V8
            // Private, and `binding::protos::shell_proto` reads that instead.
            //
            // LAST IN THIS BLOCK, and that is the whole of its contract: it must run after the final
            // install (so no prototype it records can still be replaced) and before any page script
            // (so nothing a page wrote can be in it). `install_window_named_properties` above is the
            // last install in `Engine::new`; the next JS to run in this realm is the page's, through
            // `load_page`/`evaluate_pending_modules`. Anything that mints BEFORE this point takes
            // `shell_proto`'s unchanged live-global fallback, which is what the boot-time install
            // sites do and why this needs no ordering list.
            // The registry walk's last step: every own global at the worn bundle's
            // position, after the final install and before the seal (whose contract it shares: nothing
            // creates a global after it, and no page script has run).
            crate::navtrace::mark("realm_install", t_install.elapsed().as_secs_f64() * 1000.0);
            let t_seat = std::time::Instant::now();
            // (the `constructor` seat, `seat_constructors_in_capture_order`, ran here until
            // It is deleted: the staged creates seat their own inside the recording.)
            webapi::install::installer::seat_globals_in_capture_order(scope);
            crate::navtrace::mark("seat_globals", t_seat.elapsed().as_secs_f64() * 1000.0);
            // Every realm seals its own: `shell_proto` and `proto_in_realm` read the
            // CURRENT realm's map, and the map's stamps are what a prototype is translated by.
            webapi::seal_interface_protos(scope);
            // The surface a nested context's window is built from, under the same
            // contract -- after the last install, before the first page script.
            webapi::seal_frame_surface(scope);
            // What this process recorded while the realm installed, as a blueprint
            // file for the next process of this binary (a no-op unless the binary switched the disk
            // cache on, and unless something new was recorded). After the last install, like the
            // seals above: nothing recorded later could be in it.
            webapi::persist_templates(scope);
            // The engine's own heap, the moment before the first page script: what `performance.memory` takes away
            // (`webapi::mark_heap_baseline`).
            webapi::mark_heap_baseline(scope);
}

