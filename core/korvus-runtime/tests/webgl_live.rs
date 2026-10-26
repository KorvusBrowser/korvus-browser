//! LIVE gate for the WebGL family as four `interface!` tables.
//!
//! `core/html/webgl.rs` installed 203 recorded draw calls and six accessors onto four prototypes it
//! fetched out of the generated shell registry, and `wire_canvas_oracle` then OVERWROTE four of
//! those names (`getParameter`, `getExtension`, `getSupportedExtensions`, `getContextAttributes`)
//! with the oracle-backed forms. Two writers on one slot, neither of them the installer — which is
//! what invariant I2 forbids. The rows are `webgl/tables.rs` now and each member is written once.
//!
//! The four table-cutover failure modes, one section each:
//!
//!   1. **An instance on the prototype.** Both context prototypes carry exactly the capture's member
//!      list in the capture's order — 298 and 559 WebIDL CONSTANTS included, on BOTH the prototype
//!      and the interface object, with the recorded values and the sealed descriptor.
//!   2. **A row accepting a foreign receiver.** The four implemented accessors refuse one.
//!   3. **A body reachable where it was inert, or the reverse.** The oracle-backed reads still serve
//!      the recorded device; the 23 unrecorded methods are still honest stub rows.
//!   4. **An install ORDER change.** `canvas.getContext("webgl")` still hands back one context that
//!      records, and the objects it mints wear the tables' prototypes.

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
    str_of(e, "l17-gl-probe", src)
}

/// A fresh WebGL context expression, so each probe is independent of the last.
const GL: &str = r#"document.getElementById("c").getContext("webgl")"#;

// -- 1. the capture's member list, in the capture's order ----------------------------------------

#[tokio::test]
async fn every_prototype_carries_exactly_the_captured_members_in_order() {
    let mut e = page().await;
    let fx = bundle();
    for iface in ["WebGLRenderingContext", "WebGL2RenderingContext", "WebGLActiveInfo", "WebGLShaderPrecisionFormat"] {
        let want: Vec<String> = fx["interfaces"][iface]["members"]
            .as_array()
            .unwrap_or_else(|| panic!("{iface} is absent from the capture"))
            .iter()
            .map(|m| m["name"].as_str().unwrap().to_string())
            .collect();
        let src = format!(r#"Object.getOwnPropertyNames({iface}.prototype).filter(n => n !== "constructor").join(",")"#);
        let got = str_of(&mut e, "l17-gl-order", src);
        assert_eq!(got, want.join(","), "{iface}.prototype is not the capture's member list in the capture's order");
        // ...and the interface OBJECT's statics (the constants' second holder).
        let want_statics: Vec<String> = fx["interfaces"][iface]["statics"]
            .as_array()
            .map(|a| a.iter().map(|m| m["name"].as_str().unwrap().to_string()).collect())
            .unwrap_or_default();
        let src = format!(
            r#"Object.getOwnPropertyNames({iface}).filter(n => n !== "length" && n !== "name" && n !== "prototype").join(",")"#
        );
        assert_eq!(
            str_of(&mut e, "l17-gl-statics", src),
            want_statics.join(","),
            "{iface}'s interface object is not the capture's static list in the capture's order"
        );
    }
}

/// The constants are DATA, and a table cutover that retyped them would show up here. Every recorded
/// constant's value is compared against the bundle on both holders, and the count is asserted rather
/// than spot-checked — the brief's warning was "the count and the values must be identical after".
#[tokio::test]
async fn every_recorded_constant_keeps_its_value_and_its_seal() {
    let mut e = page().await;
    let fx = bundle();
    for iface in ["WebGLRenderingContext", "WebGL2RenderingContext"] {
        let consts: Vec<(String, f64)> = fx["interfaces"][iface]["members"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["kind"] == "value")
            .filter_map(|m| Some((m["name"].as_str()?.to_string(), m["value"].as_f64()?)))
            .collect();
        assert!(consts.len() > 250, "{iface}: the capture records only {} constants", consts.len());
        let want = serde_json::to_string(&consts.iter().map(|(n, v)| serde_json::json!([n, v])).collect::<Vec<_>>()).unwrap();
        let src = format!(
            r#"(() => {{ const want = {want}; const bad = [];
                for (const [n, v] of want) {{
                    if ({iface}.prototype[n] !== v) bad.push("proto " + n + "=" + {iface}.prototype[n]);
                    if ({iface}[n] !== v) bad.push("ctor " + n + "=" + {iface}[n]);
                    const d = Object.getOwnPropertyDescriptor({iface}.prototype, n);
                    if (!d || d.writable !== false || d.configurable !== false || d.enumerable !== true)
                        bad.push("desc " + n + "=" + JSON.stringify(d));
                }}
                return bad.length + (bad.length ? ":" + bad.slice(0, 6).join(" | ") : ""); }})()"#
        );
        assert_eq!(str_of(&mut e, "l17-gl-const", src), "0", "{iface}: a constant's value or descriptor moved");
        // The COUNT, on both holders. Not a name PATTERN: six of WebGL2's recorded constants are
        // `FLOAT_MAT2x3` and its five siblings, whose `x` is lowercase, so the set is the capture's
        // own list of `kind: "value"` names and nothing else.
        let names = serde_json::to_string(&consts.iter().map(|(n, _)| n).collect::<Vec<_>>()).unwrap();
        let src = format!(
            r#"(() => {{ const want = new Set({names});
                         const p = Object.getOwnPropertyNames({iface}.prototype).filter(n => want.has(n)).length;
                         const c = Object.getOwnPropertyNames({iface}).filter(n => want.has(n)).length;
                         return p + "/" + c; }})()"#
        );
        assert_eq!(
            str_of(&mut e, "l17-gl-count", src),
            format!("{}/{}", consts.len(), consts.len()),
            "{iface}: the constant COUNT differs between the two holders or from the capture"
        );
    }
}

// -- 2. the foreign receiver ---------------------------------------------------------------------

/// The four accessors this lane implemented all carry a receiver check, because the shell stubs they
/// replace did: a readonly-attribute shell stub is checked against the prototype it was mounted on,
/// and a hand table's plain `= stub` row would NOT be (the D2 stub takes no receiver — the known
/// gap). Rather than drop four checks, the four rows have bodies.
#[tokio::test]
async fn the_implemented_accessors_refuse_a_foreign_receiver() {
    let mut e = page().await;
    for member in ["canvas", "drawingBufferWidth", "drawingBufferHeight", "drawingBufferFormat"] {
        assert_eq!(
            probe(&mut e, &format!(r#"Object.getOwnPropertyDescriptor(WebGLRenderingContext.prototype, "{member}").get.call({{}})"#)),
            "throw:TypeError:Illegal invocation",
            "WebGLRenderingContext.prototype.{member} accepted a foreign receiver"
        );
    }
    // The 23 methods with no recorded body are honest `stub` rows, NOT missing and NOT pretending:
    // present, callable, answering the stub's shape default. (They do not refuse a foreign receiver;
    // that is the D2 stub's receiver check — it is pinned here, not hidden.)
    let src = r#"(() => { const g = document.getElementById("c").getContext("webgl");
        return ["copyTexImage2D", "compressedTexImage2D"].map(n => typeof g[n]).join(","); })()"#;
    assert_eq!(str_of(&mut e, "l17-gl-stub", src.to_string()), "function,function");
}

/// The WebIDL arity guards, from the same `required` counts the registry pass used.
#[tokio::test]
async fn every_row_keeps_its_recorded_arity_guard() {
    let mut e = page().await;
    for (member, need) in [("getParameter", 1), ("getExtension", 1), ("bindAttribLocation", 3), ("vertexAttrib4f", 5)] {
        assert_eq!(
            probe(&mut e, &format!("{GL}.{member}()")),
            format!(
                "throw:TypeError:Failed to execute '{member}' on 'WebGLRenderingContext': {need} argument{} required, but only 0 present.",
                if need == 1 { "" } else { "s" }
            ),
            "WebGLRenderingContext.{member} lost its arity guard"
        );
    }
    // A `required 0` member is not wrapped.
    assert_eq!(probe(&mut e, &format!("Array.isArray({GL}.getSupportedExtensions())")), "ok:true");
}

// -- 3. the oracle-backed reads still serve the recorded device ----------------------------------

/// These four were the second writer this lane removed, so they are the members most likely to have
/// silently lost their oracle: they are `build_fn_data` over the PARSED oracle, and a table row has
/// no data channel, so the parsed block is handed over through `webgl::WebGLOracle` instead. Read
/// back off the live surface against the bundle's own recorded values.
#[tokio::test]
async fn the_oracle_backed_reads_still_answer_the_recorded_device() {
    let mut e = page().await;
    let oracle: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("oracle")).unwrap_or_default())
            .unwrap_or(serde_json::Value::Null);
    let renderer = oracle["webgl"]["unmaskedRenderer"].as_str().map(str::to_string);
    if let Some(want) = renderer {
        // UNMASKED_RENDERER_WEBGL (37446) is the single most-read fingerprinting parameter there is.
        // Asked AFTER `getExtension("WEBGL_debug_renderer_info")`: before it, Chrome 150 answers `null`
        // with INVALID_ENUM (measured -- `fixtures/p10d/webgl.chrome-150.txt`).
        assert_eq!(
            probe(&mut e, &format!(r#"({GL}.getExtension("WEBGL_debug_renderer_info"), {GL}.getParameter(37446))"#)),
            format!("ok:{want}")
        );
    }
    // ...and the shape of the other three, which need no bundle to be wrong.
    assert_eq!(probe(&mut e, &format!(r#"{GL}.getSupportedExtensions().includes("WEBGL_lose_context")"#)), "ok:true");
    assert_eq!(probe(&mut e, &format!(r#"{GL}.getExtension("no_such_extension")"#)), "ok:null");
    assert_eq!(probe(&mut e, &format!("{GL}.getContextAttributes().alpha")), "ok:true");
    assert_eq!(probe(&mut e, &format!("{GL}.getContextAttributes().stencil")), "ok:false");
    // A WebGL2 context reads its OWN parameter table, which is why the v1 and v2 rows are different
    // callbacks and the row's label is what tells them apart.
    assert_eq!(
        probe(&mut e, r#"(() => { const g = document.createElement("canvas").getContext("webgl2"); g.getExtension("WEBGL_debug_renderer_info"); return typeof g.getParameter(37446); })()"#),
        "ok:string",
    );
}

/// The four accessors this lane implemented, against the values the capture RECORDED for them
/// (`oracle/WebGLRenderingContext.json`): a context on an 8x8 canvas reports 8; resizing the element
/// moves the answer, so it is a live read; a zero-sized canvas clamps to 1; and the drawing buffer's
/// sized format follows the `alpha` attribute the page asked for.
#[tokio::test]
async fn the_drawing_buffer_reads_answer_what_chrome_recorded() {
    let mut e = page().await;
    let src = r#"(() => {
        const c = document.createElement("canvas"); c.width = 8; c.height = 8;
        const g = c.getContext("webgl");
        const a = [g.drawingBufferWidth, g.drawingBufferHeight, g.drawingBufferFormat];
        c.width = 37; c.height = 41;
        a.push(g.drawingBufferWidth, g.drawingBufferHeight);
        c.width = 0; c.height = 0;
        a.push(g.drawingBufferWidth, g.drawingBufferHeight);
        g.drawingBufferWidth = 999;               // readonly: ignored
        c.width = 8;
        a.push(g.drawingBufferWidth);
        const c2 = document.createElement("canvas");
        a.push(c2.getContext("webgl", { alpha: false }).drawingBufferFormat);
        return a.join(",");
    })()"#;
    // 32856 = 0x8058 RGBA8 (the recorded default context), 33185 is nothing; without alpha the
    // format is 0x8051 RGB8 = 32849.
    assert_eq!(str_of(&mut e, "l17-gl-buffer", src.to_string()), "8,8,32856,37,41,1,1,8,32849");
}

// -- 4. the install ORDER: one context, still recording -------------------------------------------

#[tokio::test]
async fn the_context_object_is_one_object_and_still_records() {
    let mut e = page().await;
    let src = r#"(() => {
        const c = document.getElementById("c");
        const a = c.getContext("webgl"), b = c.getContext("webgl");
        const buf = a.createBuffer();
        a.bindBuffer(a.ARRAY_BUFFER, buf);
        a.clearColor(0, 0, 0, 1);
        a.clear(a.COLOR_BUFFER_BIT);
        return [a === b,
                a instanceof WebGLRenderingContext,
                Object.prototype.toString.call(a),
                Object.getOwnPropertyNames(a).length,
                a.canvas === c,
                Object.prototype.toString.call(buf),
                Object.getOwnPropertyNames(buf).length,
                a.ARRAY_BUFFER,
                a.isBuffer(buf)].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "l17-gl-identity", src.to_string()),
        "true|true|[object WebGLRenderingContext]|0|true|[object WebGLBuffer]|0|34962|true",
    );
}

/// The two value objects `webgl.rs` mints wear the TABLES' prototypes (the mint sites read
/// `install::live::set_table_proto` now), carry their values on privates, and have no own properties
/// — which is what a real browser's have.
#[tokio::test]
async fn the_minted_value_objects_wear_the_table_prototypes() {
    let mut e = page().await;
    let src = r#"(() => {
        const g = document.getElementById("c").getContext("webgl");
        const p = g.createProgram();
        const vs = g.createShader(g.VERTEX_SHADER);
        g.shaderSource(vs, "attribute vec4 aPos; void main() { gl_Position = aPos; }");
        g.compileShader(vs);
        const fs = g.createShader(g.FRAGMENT_SHADER);
        g.shaderSource(fs, "void main() { gl_FragColor = vec4(1.0); }");
        g.compileShader(fs);
        g.attachShader(p, vs); g.attachShader(p, fs); g.linkProgram(p);
        const info = g.getActiveAttrib(p, 0);
        const spf = g.getShaderPrecisionFormat(g.FRAGMENT_SHADER, g.HIGH_FLOAT);
        return [info instanceof WebGLActiveInfo,
                Object.prototype.toString.call(info),
                Object.getOwnPropertyNames(info).length,
                info.name,
                spf instanceof WebGLShaderPrecisionFormat,
                Object.prototype.toString.call(spf),
                Object.getOwnPropertyNames(spf).length,
                spf.precision].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "l17-gl-mint", src.to_string()),
        "true|[object WebGLActiveInfo]|0|aPos|true|[object WebGLShaderPrecisionFormat]|0|23",
    );
}

/// None of the four is newable, which is what the capture records.
#[tokio::test]
async fn none_of_the_four_is_newable() {
    let mut e = page().await;
    for iface in ["WebGLRenderingContext", "WebGL2RenderingContext", "WebGLActiveInfo", "WebGLShaderPrecisionFormat"] {
        let got = probe(&mut e, &format!("new {iface}()"));
        assert!(got.starts_with("throw:TypeError:"), "new {iface}() answered {got}");
    }
}
