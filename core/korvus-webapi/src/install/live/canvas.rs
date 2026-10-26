//! The 2D canvas rendering context and the three objects it hands back.
//!
//! Written.

use deno_core::v8;

use super::rows::build_newable;
use crate::install::installer::{self, Flavor};

/// `CanvasRenderingContext2D`, `CanvasGradient`, `CanvasPattern` and `Path2D`, from their tables.
///
/// WHERE THIS RUNS, and why it is not moved. The four interfaces used to be generated registry rows
/// created by the registry install, and `canvas_2d::install` then overwrote 73 of those shell stubs
/// with native functions from the LAST step of the bootstrap (`wire_canvas_oracle`).
/// The tables take the create AND the members, so there is one install instead of two -- but it
/// happens in the same step of the same function, because that position is load-bearing in two
/// directions: `HTMLCanvasElement.getContext` is defined by the very same step (a page that calls it
/// earlier gets the same nothing it always did), and the
/// `canvas_2d::borrow_offscreen_2d` copies these prototypes' descriptors onto
/// `OffscreenCanvasRenderingContext2D` from `install_window_postshells`, which runs after.
///
/// `Path2D` is the one table here that is really NEWABLE (`new Path2D(other)` copies a segment
/// list), so it is built through [`build_newable`] over its own constructor body; the other three
/// throw "Illegal constructor", which is what the capture records (`"newable": false`).
pub fn install_window_canvas<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::html::canvas_2d::{
        cb_path2d_ctor, CANVASGRADIENT, CANVASPATTERN, CANVASRENDERINGCONTEXT2D, PATH2D, TEXTMETRICS,
    };

    let mut ledger = installer::take_ledger(scope);
    for def in [&CANVASRENDERINGCONTEXT2D, &CANVASGRADIENT, &CANVASPATTERN, &TEXTMETRICS] {
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus] canvas install gap: {}.{}", g.interface, g.member);
        }
    }
    build_newable(scope, global, &PATH2D, cb_path2d_ctor, None, None, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// The two WebGL context interfaces and the two value objects they hand back, from their tables.
///
/// WHERE THIS RUNS. `wire_canvas_oracle` again, and for a reason this family makes sharper than
/// the 2D one: the four oracle-backed rows (`getParameter`, `getExtension`,
/// `getSupportedExtensions`, `getContextAttributes`) are `build_fn_data` over the PARSED oracle, and
/// that step is where the oracle is parsed. Before this lane it ran `webgl::install` and then
/// OVERWROTE those four names with the oracle-backed versions — two writers on one slot. There is
/// one writer now: the row.
///
/// None of the four is newable (`"newable": false` in the capture), so all four go through
/// `create_interface`; `WebGLActiveInfo` and `WebGLShaderPrecisionFormat` are minted by
/// `webgl.rs` onto the prototypes this builds.
pub fn install_window_webgl<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let page = crate::core::html::browsing_context::main_global(scope) == global;
    install_webgl_interfaces(scope, global, "korvus", page);
}

/// The same four tables in the WORKER realm (invariant I8). `worker.surface.json` lists all four
/// with member lists byte-identical to the window's, and the name-based inventory took them out of
/// BOTH generated registries, so without this call the worker would lose them outright.
///
/// THE WORKER HAS NO DEVICE ORACLE, and that is not a gap this lane opens: `wire_canvas_oracle`
/// never ran in a worker, so the four oracle-backed members were worker SHELL STUBS answering
/// nothing. Their rows answer the same nothing here — `webgl::WebGLOracle` is absent, the `data` is
/// `undefined`, and each of the four callbacks already returns early on a non-object `data`.
pub fn install_worker_webgl<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    install_webgl_interfaces(scope, global, "korvus-worker", false);
}

/// Wire this realm's device oracle, the worker's half of what `install_window_bootstrap`
/// does for the window: parse the persona's `device_oracle_json` so canvas/WebGL reads answer the
/// captured device. WITHOUT IT a worker's `OffscreenCanvas.getContext("webgl")` returns null
/// (`oracle_has_webgl` is false) and a worker WebGL probe reads "NA" -- a Brave worker answers its real
/// `ANGLE (...)`/`SwiftShader` VENDOR/RENDERER in a worker too, so the absence is a bot tell
/// (deviceandbrowserinfo). Same oracle, both realms (invariant I8).
pub fn wire_worker_canvas_oracle<'s>(scope: &mut v8::PinScope<'s, '_>, oracle_json: &str) {
    // DATA ONLY -- the worker's canvas/WebGL interfaces are already installed by
    // install_worker_canvas/install_worker_webgl; wire_canvas_oracle (the WINDOW path) would re-run
    // install_window_* and mount those slots a second time, which the install ledger rejects.
    crate::core::html::html_canvas_element::wire_webgl_oracle_data_only(scope, oracle_json);
}

///
/// LAZILY ON THE PAGE: when `lazy` (the page realm) and this realm has an interface's template
/// -- built in this isolate, or in the process's blueprint: a hit -- and this device publishes it, its name goes on the global in its place as a lazy
/// interface object (`templates::put_lazy_global`), made from the template on first read, instead of instantiating
/// the ~1,000 members of the four that a page which never draws with WebGL never reads. A miss installs eagerly,
/// which records the template. Nothing else in the install reads these four: `getContext` reaches its context's
/// prototype through `protos::shell_proto`, which makes a lazy one.
fn install_webgl_interfaces<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, realm: &str, lazy: bool) {
    use crate::core::html::webgl::tables::{
        WEBGL2RENDERINGCONTEXT, WEBGLACTIVEINFO, WEBGLRENDERINGCONTEXT, WEBGLSHADERPRECISIONFORMAT,
    };

    let mut ledger = installer::take_ledger(scope);
    for def in [&WEBGLRENDERINGCONTEXT, &WEBGL2RENDERINGCONTEXT, &WEBGLACTIVEINFO, &WEBGLSHADERPRECISIONFORMAT] {
        // an interface object on the global: writable, not enumerable, configurable (`installer::publish_on_global`)
        if lazy {
            if let Some(rows) = installer::has_table_template(scope, def) {
                if installer::publishes_interface(scope, def) && crate::install::templates::put_lazy_global(scope, global, def.name, def.name, false, true) {
                    // the rows its create would have recorded: the ledger and the built surface agree
                    installer::record_lazy_rows(scope, &mut ledger, &rows);
                    crate::install::templates::count_lazy(scope);
                    if crate::install::templates::reporting() {
                        crate::install::templates::report(&format!("LAZY-PAGE put {}", def.name));
                    }
                    continue;
                }
            }
        }
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[{realm}] webgl install gap: {}.{}", g.interface, g.member);
        }
    }
    installer::put_ledger(scope, ledger);
}

/// The six WebGPU interfaces this engine can honestly answer, from their tables.
///
/// `GPUPipelineError` and `GPUUncapturedErrorEvent` are the two NEWABLE ones, and their constructors
/// have bodies for a reason the tables cannot express: each builds its base object with the REAL
/// `DOMException` / `Event` constructor and re-parents it, because those two write per-instance
/// slots (an unforgeable `isTrusted`, a `stack`) that only they can write. So both go through
/// [`build_newable`] over that body, with the WebIDL parent the capture records.
///
/// The other four throw "Illegal constructor", which is what the capture records. The ~192 members
/// of the family that stay generated shells (`GPUDevice`, `GPUBuffer`, …) are untouched: with no
/// adapter there is nothing honest for them to answer.
pub fn install_window_webgpu<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    install_webgpu_interfaces(scope, global, "korvus");
}

/// The same six tables in the WORKER realm (invariant I8). `worker.surface.json` lists all six with
/// member lists byte-identical to the window's, and the name-based inventory took them out of BOTH
/// generated registries, so without this call the worker would lose them outright. Their bodies are
/// realm-independent — none of the six reads a device oracle — so the worker gets the window's
/// answers where it used to get shell stubs.
///
/// AFTER the event model, because `GPUUncapturedErrorEvent` splices onto `Event.prototype`.
pub fn install_worker_webgpu<'s>(scope: &mut v8::PinScope<'s, '_>) {
    let global = scope.get_current_context().global(scope);
    install_webgpu_interfaces(scope, global, "korvus-worker");
    // The five WebGPU constant NAMESPACES, which the worker capture lists among its globals.
    // `worker-finalize.js` built them by hand with four wrong descriptors; these are Chrome's,
    // measured. THE WORKER ONLY, deliberately: the window capture lists them too and this realm's
    // window does not have them, but adding a window global from here would move the window's surface
    // snapshot in a commit whose job is to prove a DELETION moved nothing.
    crate::core::html::webgpu::install_gpu_namespaces(scope, global);
}

fn install_webgpu_interfaces<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, realm: &str) {
    use crate::core::html::webgpu::{
        cb_pipeline_error_ctor, cb_uncaptured_error_event_ctor, GPU, GPUCANVASCONTEXT, GPUERROR,
        GPUPIPELINEERROR, GPUUNCAPTUREDERROREVENT, WGSLLANGUAGEFEATURES,
    };

    let mut ledger = installer::take_ledger(scope);
    for def in [&GPU, &WGSLLANGUAGEFEATURES, &GPUCANVASCONTEXT, &GPUERROR] {
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[{realm}] webgpu install gap: {}.{}", g.interface, g.member);
        }
    }
    build_newable(scope, global, &GPUPIPELINEERROR, cb_pipeline_error_ctor, Some("DOMException"), None, &mut ledger);
    build_newable(scope, global, &GPUUNCAPTUREDERROREVENT, cb_uncaptured_error_event_ctor, Some("Event"), None, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// `CanvasGradient`, `CanvasPattern` and `Path2D` in the WORKER realm, from the same three tables
/// (invariant I8).
///
/// THE INVENTORY IS NAME-BASED, so a table takes its interface out of BOTH generated registries.
/// `worker.surface.json` lists these three — an `OffscreenCanvas` 2D context in a worker hands back
/// gradients and patterns and takes `Path2D`s — with member lists byte-identical to the window's, so
/// without this call the worker would lose all three outright. `CanvasRenderingContext2D` is NOT
/// here: the worker capture does not list it (the worker's 2D context is
/// `OffscreenCanvasRenderingContext2D`, still a generated worker registry interface).
///
/// The three carried shell STUBS in this realm; they carry the window's real bodies now. The shape is
/// unchanged — same names, same lengths, same descriptor flags, same WebIDL arity guards — because
/// the rows were derived from the registry rows they replace.
pub fn install_worker_canvas<'s>(scope: &mut v8::PinScope<'s, '_>) {
    use crate::core::html::canvas_2d::{cb_path2d_ctor, CANVASGRADIENT, CANVASPATTERN, PATH2D, TEXTMETRICS};

    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    for def in [&CANVASGRADIENT, &CANVASPATTERN, &TEXTMETRICS] {
        let built = installer::create_interface(scope, global, def, def.name, None, None, Flavor::Table, &mut ledger);
        for g in &built.gaps {
            eprintln!("[korvus-worker] canvas install gap: {}.{}", g.interface, g.member);
        }
    }
    build_newable(scope, global, &PATH2D, cb_path2d_ctor, None, None, &mut ledger);
    installer::put_ledger(scope, ledger);
    // The worker's 2D context has the window's bodies as its own rows (the
    // registry's `OffscreenCanvasRenderingContext2D` rows name `kits::canvas_2d`), so no donor is built.
}
