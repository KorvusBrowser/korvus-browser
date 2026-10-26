//! LIVE gate for the WebGPU family as six `interface!` tables.
//!
//! `core/html/webgpu.rs` fetched six prototypes out of the generated shell registry and defined
//! seventeen members onto them by hand — a second install mechanism beside `install/installer.rs`
//! (invariant I2), and seventeen members the install ledger never saw. All six are tables now.
//!
//! The four table-cutover failure modes, one section each:
//!
//!   1. **An instance on the prototype.** Each prototype carries exactly the capture's member list,
//!      in the capture's order, and `GPUCanvasContext.prototype` is not a live context.
//!   2. **A row accepting a foreign receiver.** Every one of the six implemented accessors refuses.
//!   3. **A body reachable where it was inert, or the reverse.** This whole family is the
//!      adapter-less answer: `requestAdapter()` resolves `null`, `configure()` always raises the
//!      WebIDL TypeError, `getConfiguration()` is `null`, `getCurrentTexture()` is an
//!      InvalidStateError and the WGSL feature set is empty. Every one is asserted here, because an
//!      "improvement" in any of them would be a fabricated device.
//!   4. **An install ORDER change.** `GPUPipelineError` still inherits `DOMException` and
//!      `GPUUncapturedErrorEvent` still inherits `Event` — and the three CONCRETE errors, which stay
//!      generated registry rows, still inherit the `GPUError` this lane moved out of the registry
//!      (the `rechain_orphans` repair, which runs later).

use engine::Engine;

const PAGE: &str = r#"<!doctype html><html><body><canvas id="c" width="8" height="8"></canvas></body></html>"#;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

fn bundle() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap()
}

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

fn probe(e: &mut Engine, expr: &str) -> String {
    let src = format!(
        r#"(() => {{ try {{ const v = ({expr}); return "ok:" + String(v); }}
                     catch (err) {{ return "throw:" + err.constructor.name + ":" + err.message; }} }})()"#
    );
    str_of(e, "l17-gpu-probe", src)
}

const IFACES: &[&str] =
    &["GPU", "WGSLLanguageFeatures", "GPUCanvasContext", "GPUError", "GPUPipelineError", "GPUUncapturedErrorEvent"];

// -- 1. the capture's member list, in the capture's order ----------------------------------------

#[tokio::test]
async fn every_prototype_carries_exactly_the_captured_members_in_order() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let want: Vec<String> = fx["interfaces"][iface]["members"]
            .as_array()
            .unwrap_or_else(|| panic!("{iface} is absent from the capture"))
            .iter()
            .map(|m| m["name"].as_str().unwrap().to_string())
            .collect();
        let src = format!(r#"Object.getOwnPropertyNames({iface}.prototype).filter(n => n !== "constructor").join(",")"#);
        assert_eq!(
            str_of(&mut e, "l17-gpu-order", src),
            want.join(","),
            "{iface}.prototype is not the capture's member list in the capture's order"
        );
        assert_eq!(
            probe(&mut e, &format!(r#"{iface}.prototype[Symbol.toStringTag]"#)),
            format!("ok:{iface}"),
            "{iface}.prototype[Symbol.toStringTag]"
        );
    }
}

/// The two NEWABLE ones construct and the four others throw, which is what the capture records —
/// and each of the two builds its base object with the REAL `DOMException` / `Event` constructor, so
/// the per-instance slots only those can write are there.
#[tokio::test]
async fn the_two_newable_interfaces_construct_and_the_four_others_throw() {
    let mut e = page().await;
    for iface in ["GPU", "WGSLLanguageFeatures", "GPUCanvasContext", "GPUError"] {
        let got = probe(&mut e, &format!("new {iface}()"));
        assert!(got.starts_with("throw:TypeError:"), "new {iface}() answered {got}");
    }
    let src = r#"(() => {
        const p = new GPUPipelineError("m", { reason: "validation" });
        const ev = new GPUUncapturedErrorEvent("uncapturederror", { error: new GPUValidationError("v") });
        return [p instanceof GPUPipelineError, p instanceof DOMException,
                p.message, p.name, p.reason,
                ev instanceof GPUUncapturedErrorEvent, ev instanceof Event,
                ev.type, ev.isTrusted, ev.error.message].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "l17-gpu-new", src.to_string()),
        "true|true|m|GPUPipelineError|validation|true|true|uncapturederror|false|v",
    );
}

// -- 2. the foreign receiver ---------------------------------------------------------------------

/// All six implemented accessors refuse a foreign receiver, which is what they did as shell stubs
/// (a readonly-attribute shell stub is receiver-checked against its prototype) and what Chrome does.
#[tokio::test]
async fn every_implemented_accessor_refuses_a_foreign_receiver() {
    let mut e = page().await;
    for (iface, member) in [
        ("GPU", "wgslLanguageFeatures"),
        ("WGSLLanguageFeatures", "size"),
        ("GPUCanvasContext", "canvas"),
        ("GPUError", "message"),
        ("GPUPipelineError", "reason"),
        ("GPUUncapturedErrorEvent", "error"),
    ] {
        assert_eq!(
            probe(&mut e, &format!(r#"Object.getOwnPropertyDescriptor({iface}.prototype, "{member}").get.call({{}})"#)),
            "throw:TypeError:Illegal invocation",
            "{iface}.prototype.{member} accepted a foreign receiver"
        );
    }
    // The setlike's five METHODS refuse one too, and the receiver check is OUTSIDE the arity check:
    // `has.call({})` with no argument is `Illegal invocation`, not the arity TypeError, which is the
    // order Chrome applies them in. All five answered for any receiver before this lane's sweep.
    for expr in [
        r#"WGSLLanguageFeatures.prototype.has.call({})"#,
        r#"WGSLLanguageFeatures.prototype.forEach.call({})"#,
        r#"WGSLLanguageFeatures.prototype.keys.call({})"#,
        r#"WGSLLanguageFeatures.prototype.values.call({})"#,
        r#"WGSLLanguageFeatures.prototype.entries.call({})"#,
    ] {
        assert_eq!(probe(&mut e, expr), "throw:TypeError:Illegal invocation", "{expr}");
    }
    // The prototype chain alone is NOT the brand, which is also what Chrome answers.
    assert_eq!(
        probe(&mut e, "Object.create(WGSLLanguageFeatures.prototype).size"),
        "throw:TypeError:Illegal invocation",
    );
}

/// `GPUCanvasContext.configure` is the family's one row with a WebIDL required argument.
#[tokio::test]
async fn the_one_required_argument_row_keeps_its_arity_guard() {
    let mut e = page().await;
    assert_eq!(
        probe(&mut e, r#"document.getElementById("c").getContext("webgpu").configure()"#),
        "throw:TypeError:Failed to execute 'configure' on 'GPUCanvasContext': 1 argument required, but only 0 present.",
    );
}

// -- 3. the adapter-less answers, which must not "improve" ---------------------------------------

/// Every one of these is the honest answer for a machine with NO GPU adapter, and every one of them
/// would be a fabricated device if it changed. The recorded browser is itself adapter-less, so these
/// are the capture's answers, not a limitation being papered over.
#[tokio::test]
async fn the_adapter_less_answers_are_unchanged() {
    let mut e = page().await;
    assert_eq!(probe(&mut e, "navigator.gpu.getPreferredCanvasFormat()"), "ok:rgba8unorm");
    assert_eq!(probe(&mut e, "Object.prototype.toString.call(navigator.gpu.requestAdapter())"), "ok:[object Promise]");
    // The WGSL feature set is Chrome 150's shipped compiler's: a fact of the
    // browser build, not of a GPU -- stock Chrome 150 lists the same ten with no adapter, measured
    // (`gen/blink/webgpu/wgsl_language_features.json`).
    let src = r#"(() => {
        const w = navigator.gpu.wgslLanguageFeatures;
        return [w === navigator.gpu.wgslLanguageFeatures,            // the same object every read
                Object.prototype.toString.call(w),
                w.size,
                w.has("readonly_and_readwrite_storage_textures"),
                [...w].length,
                w[Symbol.iterator] === w.values,
                Object.getOwnPropertyDescriptor(WGSLLanguageFeatures.prototype, Symbol.iterator).enumerable,
                Object.prototype.toString.call(w.keys())].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "l17-gpu-wgsl", src.to_string()),
        "true|[object WGSLLanguageFeatures]|10|true|10|true|false|[object WGSLLanguageFeatures Iterator]",
    );
    // The canvas context: never configurable, so never configured.
    let src = r#"(() => {
        const ctx = document.getElementById("c").getContext("webgpu");
        let cfg = "", tex = "";
        try { ctx.configure({ device: {}, format: "rgba8unorm" }); } catch (err) { cfg = err.constructor.name; }
        try { ctx.getCurrentTexture(); } catch (err) { tex = err.name; }
        return [Object.prototype.toString.call(ctx),
                ctx.canvas === document.getElementById("c"),
                Object.getOwnPropertyNames(ctx).length,
                cfg, String(ctx.getConfiguration()), tex,
                String(ctx.unconfigure())].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "l17-gpu-ctx", src.to_string()),
        "[object GPUCanvasContext]|true|0|TypeError|null|InvalidStateError|undefined",
    );
}

// -- 4. the two inheritance chains, and the three orphans this lane created ----------------------

/// `GPUError` left the generated registry with its table, so the registry's own chain pass — which
/// runs EARLIER, from the registry install (`op_install_registry`, deleted when the ESM
/// epilogue became Rust) — could not find the parent of `GPUValidationError`,
/// `GPUOutOfMemoryError` and `GPUInternalError`, all three of which are still registry rows. The
/// repair is `install::live::rows::rechain_orphans`, which `install_window_postshells` already runs
/// over the whole registry; this asserts the repaired chain instead of assuming it — on BOTH halves,
/// the prototype chain and the interface-object chain.
#[tokio::test]
async fn the_three_concrete_errors_still_inherit_the_table_gpuerror() {
    let mut e = page().await;
    for iface in ["GPUValidationError", "GPUOutOfMemoryError", "GPUInternalError"] {
        let src = format!(
            r#"(() => {{ const err = new {iface}("boom");
                         return [Object.getPrototypeOf({iface}.prototype) === GPUError.prototype,
                                 Object.getPrototypeOf({iface}) === GPUError,
                                 err instanceof GPUError,
                                 err.message].join("|"); }})()"#
        );
        assert_eq!(
            str_of(&mut e, "l17-gpu-chain", src),
            "true|true|true|boom",
            "{iface} lost its inheritance from the table-owned GPUError"
        );
    }
}
