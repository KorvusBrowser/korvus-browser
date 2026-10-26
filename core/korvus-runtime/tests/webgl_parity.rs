//! GS.4 (device-oracle, WebGL slice of DoD-4): a createElement-made canvas serves the shipped bundle's
//! recorded GPU -- getContext('webgl'|'webgl2') is non-null, getParameter reads the recorded values,
//! the GL constants (gl.MAX_TEXTURE_SIZE = 3379, ...) inherit from the WebGL(2)RenderingContext
//! shells. WHICH GPU is read from the bundle's `deviceOracle` block at test time: these three tests
//! spelled out `Apple M3 Pro`, an extension count of 36 and a WebGL2 version string, and the shipped
//! bundle became a Linux/AMD capture. Two of the three then found a real gap -- the WebGL2 context
//! answered `undefined` for its VERSION because the promoter never carried the capture's WebGL2 half. `toDataURL` no longer belongs to the oracle at all -- it serializes
//! the canvas's own pixels now (see `to_data_url_serializes_what_was_drawn` at the bottom of this file),
//! so what is asserted about it here is only that a WebGL canvas still produces a real PNG of its own size.
use engine::Engine;
use spec::Persona;

/// The `deviceOracle.webgl` block of the shipped bundle, as JSON.
/// The `webgl` block a page reads: the persona's recording with this PC's GPU's values in it.
fn oracle_webgl() -> serde_json::Value {
    let p = persona::CapturedPersona::default();
    crate::webgl_device::page_webgl(&p.device_oracle_json())
}
fn s(v: &serde_json::Value) -> String { v.as_str().unwrap_or_default().to_string() }
fn has(list: &serde_json::Value, name: &str) -> bool {
    list.as_array().map(|a| a.iter().any(|e| e.as_str() == Some(name))).unwrap_or(false)
}

#[tokio::test]
async fn webgl_oracle_matches_captured_device() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("gl", r#"
        const c = document.createElement("canvas");
        const gl = c.getContext("webgl");
        print("nonNull=" + (gl !== null) + " instanceof=" + (gl instanceof WebGLRenderingContext));
        print("vendor=" + gl.getParameter(gl.VENDOR) + "|" + gl.getParameter(gl.RENDERER));
        print("version=" + gl.getParameter(gl.VERSION));
        print("maxTex=" + gl.getParameter(gl.MAX_TEXTURE_SIZE));
        const dbg = gl.getExtension("WEBGL_debug_renderer_info");
        print("unmasked=" + gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL) + "|" + gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL));
        const exts = gl.getSupportedExtensions();
        print("ext=" + exts.length + "," + (exts.indexOf("EXT_color_buffer_float") !== -1) + "," + (gl.getExtension("EXT_color_buffer_float") !== null) + "," + (gl.getExtension("NO_SUCH") === null));
        // A canvas hands back the SAME context object every call, and `experimental-webgl` names the
        // same type -- both verified against a real Brave 151, which is also where the next line's rule
        // comes from: a canvas has ONE context, so asking this one for webgl2 is null, and webgl2 needs
        // its own canvas. This test used to call `c.getContext("webgl2")` on the webgl canvas and read
        // a version string off the result, which no browser would have answered.
        print("refetch=" + (c.getContext("webgl") === gl) + "," + (c.getContext("experimental-webgl") === gl));
        print("oneCtx=" + (c.getContext("webgl2") === null) + "," + (c.getContext("2d") === null));
        const vp = gl.getParameter(gl.MAX_VIEWPORT_DIMS);
        print("vp=" + (vp instanceof Int32Array) + ":" + vp[0] + "x" + vp[1]);
        const gl2 = document.createElement("canvas").getContext("webgl2");
        print("v2=" + (gl2 instanceof WebGL2RenderingContext) + "," + gl2.getParameter(gl2.VERSION));
        print("attrs=" + gl.getContextAttributes().antialias + "," + gl.getContextAttributes().alpha);
        // getContext/getParameter must be NATIVE (a JS shim is a stealth tell better detectors check).
        print("native=" + (c.getContext.toString().includes("[native code]")) + "," + (gl.getParameter.toString().includes("[native code]")));
        const u = c.toDataURL();
        // The metadata above is recorded replay and still is (R3 has not landed, so there are no GL
        // pixels). The SERIALIZATION is not: a WebGL canvas now returns a real PNG of its own size,
        // transparent because nothing rasters GL yet, instead of the captured device's 280x60 image.
        // Honest and, unlike a constant, not identical for two canvases of different sizes.
        print("dataurl=" + u.startsWith("data:image/png;base64,iVBOR"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    let w = oracle_webgl();
    let exts = &w["extensions"];
    let n_ext = exts.as_array().map(|a| a.len()).unwrap_or(0);
    let cbf = has(exts, "EXT_color_buffer_float");
    let max_tex = w["paramsByNum"]["3379"].as_i64().expect("MAX_TEXTURE_SIZE recorded");
    let vp = &w["paramsByNum"]["3386"];
    let (vp0, vp1) = (vp[0].as_i64().unwrap_or(0), vp[1].as_i64().unwrap_or(0));
    assert!(!s(&w["versionV2"]).is_empty(), "the bundle must carry the WebGL2 VERSION string");
    assert_eq!(e.take_output(), vec![
        "nonNull=true instanceof=true".to_string(),
        format!("vendor={}|{}", s(&w["vendor"]), s(&w["renderer"])),
        format!("version={}", s(&w["versionV1"])),
        format!("maxTex={max_tex}"),
        format!("unmasked={}|{}", s(&w["unmaskedVendor"]), s(&w["unmaskedRenderer"])),
        format!("ext={n_ext},{cbf},{cbf},true"),
        "refetch=true,true".to_string(),
        "oneCtx=true,true".to_string(),
        format!("vp=true:{vp0}x{vp1}"),
        format!("v2=true,{}", s(&w["versionV2"])),
        "attrs=true,true".to_string(),
        "native=true,true".to_string(),
        "dataurl=true".to_string(),
    ]);
}

#[tokio::test]
async fn parsed_canvas_serves_webgl() {
    // The native prototype methods cover a canvas that came from HTML parsing (not createElement) --
    // the instance-level approach could not. This is why getContext lives on the prototype.
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body><canvas id=c></canvas></body></html>").unwrap();
    e.eval("p", r#"
        const c = document.getElementById("c");
        const gl = c.getContext("webgl");
        const dbg = gl.getExtension("WEBGL_debug_renderer_info");
        print("renderer=" + gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL));
        print("img=" + c.toDataURL().startsWith("data:image/png;base64,iVBOR"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        format!("renderer={}", s(&oracle_webgl()["unmaskedRenderer"])),
        "img=true".to_string(),
    ]);
}

#[tokio::test]
async fn no_gpu_persona_has_no_webgl() {
    // A persona with no captured device -> getContext('webgl') is null (honest, like the old stub),
    // so the presence of WebGL is persona-driven, never fabricated. The persona is the shipped
    // bundle MINUS its oracle -- this ran on the default persona after the test-persona sweep, and
    // the shipped device has a GPU, which made the test assert the opposite of its own name.
    let mut e = Engine::new(persona::Overridden::new().without_device_oracle().boxed());
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("g", r#"
        const c = document.createElement("canvas");
        print("webgl=" + (c.getContext("webgl") === null) + " 2d=" + (c.getContext("2d") !== null));
        // A canvas nothing drew on serializes to a real, transparent PNG of its own size -- not to
        // the recorded oracle's image, and not to the bare "data:image/png;base64," this used to
        // assert, which is a data URL containing no PNG at all.
        const u = c.toDataURL();
        print("png=" + u.startsWith("data:image/png;base64,iVBOR"));
        const big = document.createElement("canvas");
        big.width = 600; big.height = 400;
        print("sizeMatters=" + (big.toDataURL().length !== u.length));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "webgl=true 2d=true",
        "png=true",
        "sizeMatters=true",
    ]);
}

/// `toDataURL` serializes what the page drew, and a canvas owns one context.
///
/// Both halves used to be false at once, and they were the same defect seen from two sides.
/// `toDataURL` was `rv.set(args.data())` -- one PNG recorded from the captured device, handed back
/// for every canvas of every size forever -- and `getContext` built a fresh object per call, so the
/// display list (keyed by the context) was unreachable from the canvas that would have to serialize
/// it. Fixing the first required fixing the second.
///
/// What is asserted here is deliberately not a byte count. The PNG bytes depend on the encoder build
/// (Chrome 151 uses `SkPngRustEncoder`; skia-safe 0.99's prebuilt has only `SkPngEncoder`), and
/// pinning a length here would turn an honest encoder upgrade into a test failure. What must hold is
/// what a fingerprinting script actually measures: the output changes when the drawing changes,
/// changes when the size changes, and agrees with `getImageData` about what is on the canvas.
#[tokio::test]
async fn to_data_url_serializes_what_was_drawn() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("d", r##"
        const draw = (text) => {
            const c = document.createElement("canvas");
            c.width = 200; c.height = 40;
            const x = c.getContext("2d");
            x.font = "14px Arial";
            x.fillStyle = "#069";
            x.fillText(text, 2, 20);
            return c.toDataURL();
        };
        // The classic probe: the page picks the content, then hashes the string.
        print("content=" + (draw("alpha") !== draw("beta")));
        // Two canvases of different sizes cannot serialize identically. They did.
        const size = (w, h) => { const c = document.createElement("canvas"); c.width = w; c.height = h;
                                 c.getContext("2d"); return c.toDataURL(); };
        print("size=" + (size(10, 10) !== size(100, 100)));
        // A canvas has ONE context, and it is the same object every call -- so drawing through one
        // handle is visible through another, which is how any page that fetches the context twice works.
        const c = document.createElement("canvas");
        const a = c.getContext("2d");
        print("identity=" + (a === c.getContext("2d")) + " second=" + (c.getContext("webgl") === null));
        a.fillStyle = "#ff0000"; a.fillRect(0, 0, 10, 10);
        print("shared=" + JSON.stringify(Array.from(c.getContext("2d").getImageData(0, 0, 1, 1).data)));
        // The cross-check a detector runs: the two readback paths must describe one canvas.
        const w = document.createElement("canvas"); w.width = 8; w.height = 8;
        const wx = w.getContext("2d"); wx.fillStyle = "#ffffff"; wx.fillRect(0, 0, 8, 8);
        const blank = document.createElement("canvas"); blank.width = 8; blank.height = 8;
        print("cohere=" + (wx.getImageData(0,0,1,1).data[0] === 255 && w.toDataURL() !== blank.toDataURL()));
        // HTML: a canvas with no pixels serializes to "data:," -- not to a 300x150 default.
        const z = document.createElement("canvas"); z.width = 0;
        print("zero=" + z.toDataURL());
        // The type argument is honoured, and quality moves the size in the right direction.
        const j = document.createElement("canvas"); j.width = 120; j.height = 60;
        const jx = j.getContext("2d");
        for (let i = 0; i < 60; i++) { jx.fillStyle = "rgb(" + (i*4) + "," + (255-i*4) + "," + (i*3) + ")"; jx.fillRect(i*2, 0, 2, 60); }
        print("jpeg=" + j.toDataURL("image/jpeg").startsWith("data:image/jpeg;base64,")
              + " quality=" + (j.toDataURL("image/jpeg", 1.0).length > j.toDataURL("image/jpeg", 0.1).length));
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "content=true",
        "size=true",
        "identity=true second=true",
        "shared=[255,0,0,255]",
        "cohere=true",
        "zero=data:,",
        "jpeg=true quality=true",
    ]);
}

// ── R3: the pixels ────────────────────────────────────────────────────────────────────────────
//
// Everything above this line is the recorded device METADATA, which was already right. Everything
// below is the half that did not exist: at HEAD, a WebGL context drew nothing, `readPixels` wrote
// zeroes, `createBuffer()` returned `undefined`, and two different `clearColor` values produced
// byte-identical 174-byte `toDataURL` strings.

/// The three-line discriminator the R3 brief opens with.
#[tokio::test]
async fn two_clear_colours_serialize_differently() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("gl", r#"
        const c = document.createElement("canvas");
        c.width = 32; c.height = 32;
        const gl = c.getContext("webgl");
        gl.clearColor(1, 0, 0, 1); gl.clear(gl.COLOR_BUFFER_BIT);
        const red = c.toDataURL();
        gl.clearColor(0, 0, 1, 1); gl.clear(gl.COLOR_BUFFER_BIT);
        const blue = c.toDataURL();
        print("differ=" + (red !== blue));
        // ...and the difference is not merely "some bytes changed": the pixels are the colours.
        const px = new Uint8Array(4);
        gl.readPixels(16, 16, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
        print("blue=" + Array.from(px).join(","));
        gl.clearColor(0, 1, 0, 1); gl.clear(gl.COLOR_BUFFER_BIT);
        gl.readPixels(16, 16, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
        print("green=" + Array.from(px).join(","));
        print("png=" + red.startsWith("data:image/png;base64,iVBOR"));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "differ=true",
        "blue=0,0,255,255",
        "green=0,255,0,255",
        "png=true",
    ]);
}

/// `toDataURL` and `readPixels` describe ONE buffer. The 2D path had exactly this bug once
/// (`getImageData` rastered live while `toDataURL` replayed a recording) and it was a
/// single-expression tell; this is the same cross-check on the GL path.
#[tokio::test]
async fn read_pixels_and_to_data_url_agree() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("gl", r#"
        const c = document.createElement("canvas");
        c.width = 8; c.height = 8;
        const gl = c.getContext("webgl", { antialias: false });
        // A scissored clear, so the buffer is NOT one flat colour and a coincidental match is out.
        gl.clearColor(1, 0, 0, 1); gl.clear(gl.COLOR_BUFFER_BIT);
        gl.enable(gl.SCISSOR_TEST);
        gl.scissor(0, 0, 4, 8);
        gl.clearColor(0, 0, 1, 1); gl.clear(gl.COLOR_BUFFER_BIT);
        const px = new Uint8Array(8 * 8 * 4);
        gl.readPixels(0, 0, 8, 8, gl.RGBA, gl.UNSIGNED_BYTE, px);
        // readPixels counts rows from the BOTTOM; a 2D canvas drawing the same PNG counts from the
        // top. Compare through a 2D context that has drawn the WebGL canvas.
        const d = document.createElement("canvas");
        d.width = 8; d.height = 8;
        const x = d.getContext("2d");
        x.drawImage(c, 0, 0);
        const img = x.getImageData(0, 0, 8, 8).data;
        let same = true;
        for (let row = 0; row < 8; row++) {
            for (let col = 0; col < 8; col++) {
                const a = ((7 - row) * 8 + col) * 4;   // readPixels row `7-row` is image row `row`
                const b = (row * 8 + col) * 4;
                for (let k = 0; k < 4; k++) { if (px[a + k] !== img[b + k]) same = false; }
            }
        }
        print("agree=" + same);
        print("left=" + Array.from(px.slice(0, 4)).join(",") + " right=" + Array.from(px.slice(28, 32)).join(","));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "agree=true",
        "left=0,0,255,255 right=255,0,0,255",
    ]);
}

/// The object lifecycle: `createBuffer`/`createShader`/`createProgram`/`createTexture` returned
/// `undefined` at HEAD, so nothing downstream of them could ever work.
#[tokio::test]
async fn the_object_lifecycle_returns_real_objects() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("gl", r#"
        const gl = document.createElement("canvas").getContext("webgl");
        const b = gl.createBuffer(), s = gl.createShader(gl.VERTEX_SHADER);
        const p = gl.createProgram(), t = gl.createTexture();
        const f = gl.createFramebuffer(), r = gl.createRenderbuffer();
        print("kinds=" + (b instanceof WebGLBuffer) + "," + (s instanceof WebGLShader) + ","
              + (p instanceof WebGLProgram) + "," + (t instanceof WebGLTexture) + ","
              + (f instanceof WebGLFramebuffer) + "," + (r instanceof WebGLRenderbuffer));
        // A real WebGL object has NO own properties -- the id lives on a v8 Private.
        print("shape=" + Object.getOwnPropertyNames(b).length + "," + Object.keys(b).length
              + "," + Object.getOwnPropertySymbols(b).length);
        print("distinct=" + (gl.createBuffer() !== b));
        print("is=" + gl.isBuffer(b) + "," + gl.isShader(s) + "," + gl.isProgram(p) + "," + gl.isTexture(t));
        gl.deleteBuffer(b);
        print("deleted=" + gl.isBuffer(b));
        print("badShader=" + (gl.createShader(0) === null) + " err=" + (gl.getError() === gl.INVALID_ENUM));
        print("native=" + gl.createBuffer.toString().includes("[native code]")
              + " arity=" + gl.drawArrays.length + "," + gl.texImage2D.length + "," + gl.readPixels.length);
        print("tag=" + Object.prototype.toString.call(b));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "kinds=true,true,true,true,true,true",
        "shape=0,0,0",
        "distinct=true",
        // A buffer and a texture are not OBJECTS until they are first bound; a shader and a program
        // are at creation. Chrome 150 over a loopback page, both context versions: `isBuffer`,
        // `isTexture`, `isFramebuffer`, `isRenderbuffer`, `isVertexArray` false then true after the
        // bind. This read `true,true,true,true` until then.
        "is=false,true,true,false",
        "deleted=false",
        "badShader=true err=true",
        "native=true arity=3,6,7",
        "tag=[object WebGLBuffer]",
    ]);
}

/// The whole pipeline, as a page writes it: compile, link, upload, draw, read back. Every answer
/// here comes from the recorded command list being EXECUTED -- the compile status is a real GLSL
/// compiler's, the attribute location is the linker's, and the pixel is the fragment shader's.
#[tokio::test]
async fn a_compiled_program_draws_and_reads_back() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("gl", r#"
        const c = document.createElement("canvas");
        c.width = 16; c.height = 16;
        const gl = c.getContext("webgl", { antialias: false });
        const vs = gl.createShader(gl.VERTEX_SHADER);
        gl.shaderSource(vs, "attribute vec2 a; varying vec2 v;" +
                            "void main(){ v = a * 0.5 + 0.5; gl_Position = vec4(a, 0.0, 1.0); }");
        gl.compileShader(vs);
        const fs = gl.createShader(gl.FRAGMENT_SHADER);
        gl.shaderSource(fs, "precision mediump float; varying vec2 v; uniform vec3 u;" +
                            "void main(){ gl_FragColor = vec4(v.x * u.r, v.y * u.g, u.b, 1.0); }");
        gl.compileShader(fs);
        print("compiled=" + gl.getShaderParameter(vs, gl.COMPILE_STATUS)
              + "," + gl.getShaderParameter(fs, gl.COMPILE_STATUS)
              + " log=" + JSON.stringify(gl.getShaderInfoLog(vs)));
        const p = gl.createProgram();
        gl.attachShader(p, vs); gl.attachShader(p, fs); gl.linkProgram(p); gl.useProgram(p);
        print("linked=" + gl.getProgramParameter(p, gl.LINK_STATUS)
              + " attrs=" + gl.getProgramParameter(p, gl.ACTIVE_ATTRIBUTES)
              + " unis=" + gl.getProgramParameter(p, gl.ACTIVE_UNIFORMS));
        const loc = gl.getAttribLocation(p, "a");
        print("loc=" + loc + " missing=" + gl.getAttribLocation(p, "zzz")
              + " uloc=" + (gl.getUniformLocation(p, "u") !== null)
              + "," + (gl.getUniformLocation(p, "zzz") === null));
        const info = gl.getActiveAttrib(p, 0);
        print("active=" + info.name + "," + info.size + "," + (info.type === gl.FLOAT_VEC2));
        const buf = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, buf);
        gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1,-1, 1,-1, -1,1, -1,1, 1,-1, 1,1]), gl.STATIC_DRAW);
        gl.enableVertexAttribArray(loc);
        gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
        gl.uniform3f(gl.getUniformLocation(p, "u"), 1.0, 1.0, 0.5);
        gl.drawArrays(gl.TRIANGLES, 0, 6);
        const px = new Uint8Array(4);
        // Bottom-left of the viewport: the varying is (0,0) there, so red and green are 0 and blue
        // is the uniform's 0.5.
        gl.readPixels(0, 0, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
        print("bl=" + Array.from(px).join(","));
        gl.readPixels(15, 15, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
        print("tr=" + Array.from(px).join(","));
        print("err=" + gl.getError());
        // The same program with a different uniform gives a different frame -- the pixels depend on
        // the program's inputs, not just on which ops were called.
        const a = c.toDataURL();
        gl.uniform3f(gl.getUniformLocation(p, "u"), 0.0, 1.0, 0.0);
        gl.drawArrays(gl.TRIANGLES, 0, 6);
        print("uniformMatters=" + (c.toDataURL() !== a));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "compiled=true,true log=\"\"",
        "linked=true attrs=1 unis=1",
        "loc=0 missing=-1 uloc=true,true",
        "active=a,1,true",
        // 0.5 is stored as 127 -- an exact tie goes down on the persona's GPU; stock Chrome 150 answers these
        // eight lines exactly (ANGLE, RTX 4060)
        "bl=8,8,127,255",
        "tr=247,247,127,255",
        "err=0",
        "uniformMatters=true",
    ]);
}

/// A shader that does not compile must SAY so, with a log — and the program that attaches it must
/// fail to link. A context that answered `true` to `COMPILE_STATUS` for anything would be caught the
/// first time a page fed it a deliberate syntax error, which is what shader-cache probes do.
#[tokio::test]
async fn a_broken_shader_fails_to_compile_and_to_link() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("gl", r#"
        const gl = document.createElement("canvas").getContext("webgl");
        const s = gl.createShader(gl.VERTEX_SHADER);
        gl.shaderSource(s, "void main( { gl_Position = ");
        gl.compileShader(s);
        print("status=" + gl.getShaderParameter(s, gl.COMPILE_STATUS));
        print("log=" + (gl.getShaderInfoLog(s).indexOf("ERROR") === 0));
        print("source=" + (gl.getShaderSource(s) === "void main( { gl_Position = "));
        print("type=" + (gl.getShaderParameter(s, gl.SHADER_TYPE) === gl.VERTEX_SHADER));
        const p = gl.createProgram();
        gl.attachShader(p, s); gl.linkProgram(p);
        print("link=" + gl.getProgramParameter(p, gl.LINK_STATUS)
              + " plog=" + (gl.getProgramInfoLog(p).length > 0)
              + " attached=" + gl.getProgramParameter(p, gl.ATTACHED_SHADERS));
        gl.useProgram(p);
        gl.drawArrays(gl.TRIANGLES, 0, 3);
        print("err=" + (gl.getError() === gl.INVALID_OPERATION) + " cleared=" + gl.getError());
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "status=false",
        "log=true",
        "source=true",
        "type=true",
        "link=false plog=true attached=1",
        "err=true cleared=0",
    ]);
}

/// Sub-unit (a) on its own terms: the calls a page issues reach the command list, in order, with
/// their arguments. Read from Rust — a JS-visible way to ask would be a global no browser has.
#[tokio::test]
async fn the_recorded_command_list_holds_the_draw_calls() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("gl", r#"
        const gl = document.createElement("canvas").getContext("webgl");
        gl.clearColor(0.25, 0.5, 0.75, 1);
        gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
        gl.viewport(0, 0, 64, 32);
        gl.enable(gl.DEPTH_TEST);
        gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
        const b = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, b);
        gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 1, 2]), gl.STATIC_DRAW);
        // A draw with no program in use is Blink's INVALID_OPERATION and never reaches GL
        // (ValidateRenderingState), so the draws below run under a linked one.
        const vs = gl.createShader(gl.VERTEX_SHADER);
        gl.shaderSource(vs, "attribute vec4 p; void main() { gl_Position = p; }");
        gl.compileShader(vs);
        const fs = gl.createShader(gl.FRAGMENT_SHADER);
        gl.shaderSource(fs, "void main() { gl_FragColor = vec4(1.0); }");
        gl.compileShader(fs);
        const pr = gl.createProgram();
        gl.attachShader(pr, vs); gl.attachShader(pr, fs); gl.linkProgram(pr); gl.useProgram(pr);
        gl.drawArrays(gl.TRIANGLE_STRIP, 1, 3);
        gl.drawElements(gl.LINES, 6, gl.UNSIGNED_SHORT, 12);
    "#.into()).unwrap();
    e.run().await.unwrap();
    let log = e.webgl_command_log();
    let has = |needle: &str| log.iter().any(|l| l.contains(needle));
    assert!(has("ClearColor { r: 0.25, g: 0.5, b: 0.75, a: 1.0 }"), "{log:#?}");
    assert!(has("Clear { mask: 16640 }"), "{log:#?}");
    assert!(has("Viewport { x: 0, y: 0, width: 64, height: 32 }"), "{log:#?}");
    assert!(has("Enable { cap: 2929 }"), "{log:#?}");
    assert!(has("BlendFuncSeparate { src_rgb: 770, dst_rgb: 771, src_alpha: 770, dst_alpha: 771 }"), "{log:#?}");
    // The buffer upload carries the BYTES a Float32Array holds, not its element count.
    assert!(has("BufferData { target: 34962, size: 12"), "{log:#?}");
    assert!(has("DrawArrays { mode: 5, first: 1, count: 3 }"), "{log:#?}");
    assert!(has("DrawElements { mode: 1, count: 6, ty: 5123, offset: 12 }"), "{log:#?}");
    // ...and in the order the page called them.
    let pos = |needle: &str| log.iter().position(|l| l.contains(needle)).unwrap();
    assert!(pos("ClearColor") < pos("Clear {"), "{log:#?}");
    assert!(pos("BufferData") < pos("DrawArrays"), "{log:#?}");
    assert!(pos("DrawArrays") < pos("DrawElements"), "{log:#?}");
}

/// `getContextAttributes()` describes the buffer the rasterizer actually produced. Reading the
/// attribute and then measuring the thing it claims is a two-line cross-check.
#[tokio::test]
async fn context_attributes_describe_the_buffer_that_was_drawn() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("gl", r#"
        // The default dictionary is the captured device's, unchanged.
        const d = document.createElement("canvas").getContext("webgl").getContextAttributes();
        print("defaults=" + JSON.stringify(d));
        // `alpha: false` means the drawing buffer has no alpha channel -- and the pixels say so.
        const c = document.createElement("canvas");
        c.width = 4; c.height = 4;
        const gl = c.getContext("webgl", { alpha: false, antialias: false });
        print("asked=" + gl.getContextAttributes().alpha + "," + gl.getContextAttributes().antialias);
        gl.clearColor(0, 0, 0, 0); gl.clear(gl.COLOR_BUFFER_BIT);
        const px = new Uint8Array(4);
        gl.readPixels(0, 0, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
        print("opaque=" + (px[3] === 255));
        const t = document.createElement("canvas");
        t.width = 4; t.height = 4;
        const g2 = t.getContext("webgl", { alpha: true });
        g2.clearColor(0, 0, 0, 0); g2.clear(g2.COLOR_BUFFER_BIT);
        g2.readPixels(0, 0, 1, 1, g2.RGBA, g2.UNSIGNED_BYTE, px);
        print("transparent=" + (px[3] === 0));
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "defaults={\"alpha\":true,\"antialias\":true,\"depth\":true,\"desynchronized\":false,\"failIfMajorPerformanceCaveat\":false,\"powerPreference\":\"default\",\"premultipliedAlpha\":true,\"preserveDrawingBuffer\":false,\"stencil\":false,\"xrCompatible\":false}",
        "asked=false,false",
        "opaque=true",
        "transparent=true",
    ]);
}

/// A texture uploaded from a page, sampled by a shader, read back. The upload path is its own
/// surface (`texImage2D` has two argument forms and an unpack flag) and it feeds the same buffer.
#[tokio::test]
async fn a_texture_uploaded_by_the_page_is_sampled_back() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("gl", r#"
        const c = document.createElement("canvas");
        c.width = 8; c.height = 8;
        const gl = c.getContext("webgl", { antialias: false });
        const sh = (t, src) => { const s = gl.createShader(t); gl.shaderSource(s, src); gl.compileShader(s); return s; };
        const p = gl.createProgram();
        gl.attachShader(p, sh(gl.VERTEX_SHADER,
            "attribute vec2 a; varying vec2 v; void main(){ v = a * 0.5 + 0.5; gl_Position = vec4(a, 0.0, 1.0); }"));
        gl.attachShader(p, sh(gl.FRAGMENT_SHADER,
            "precision mediump float; varying vec2 v; uniform sampler2D s; void main(){ gl_FragColor = texture2D(s, v); }"));
        gl.linkProgram(p); gl.useProgram(p);
        const b = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, b);
        gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1,-1, 1,-1, -1,1, -1,1, 1,-1, 1,1]), gl.STATIC_DRAW);
        const loc = gl.getAttribLocation(p, "a");
        gl.enableVertexAttribArray(loc);
        gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
        const tex = gl.createTexture();
        gl.bindTexture(gl.TEXTURE_2D, tex);
        // 2x2: bottom row magenta/yellow, top row black/white.
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 2, 2, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array([
            255,0,255,255,  255,255,0,255,
            0,0,0,255,      255,255,255,255,
        ]));
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
        print("param=" + (gl.getTexParameter(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER) === gl.NEAREST));
        gl.uniform1i(gl.getUniformLocation(p, "s"), 0);
        gl.drawArrays(gl.TRIANGLES, 0, 6);
        const px = new Uint8Array(4);
        gl.readPixels(1, 1, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
        print("bl=" + Array.from(px).join(","));
        gl.readPixels(6, 6, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
        print("tr=" + Array.from(px).join(","));
        print("err=" + gl.getError());
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "param=true",
        "bl=255,0,255,255",
        "tr=255,255,255,255",
        "err=0",
    ]);
}

/// WebGL 2: its own context type, its own extra surface, the same pipeline underneath.
#[tokio::test]
async fn webgl2_records_and_draws_too() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e.eval("gl", r##"
        const c = document.createElement("canvas");
        c.width = 8; c.height = 8;
        const gl = c.getContext("webgl2", { antialias: false });
        const vao = gl.createVertexArray();
        print("vao=" + (vao instanceof WebGLVertexArrayObject) + "," + gl.isVertexArray(vao));
        gl.bindVertexArray(vao);
        const sh = (t, src) => { const s = gl.createShader(t); gl.shaderSource(s, src); gl.compileShader(s); return s; };
        const p = gl.createProgram();
        gl.attachShader(p, sh(gl.VERTEX_SHADER,
            "#version 300 es\nin vec2 a; void main(){ gl_Position = vec4(a, 0.0, 1.0); }"));
        gl.attachShader(p, sh(gl.FRAGMENT_SHADER,
            "#version 300 es\nprecision mediump float; out vec4 o; void main(){ o = vec4(0.0, 1.0, 0.0, 1.0); }"));
        gl.linkProgram(p);
        print("linked=" + gl.getProgramParameter(p, gl.LINK_STATUS));
        gl.useProgram(p);
        const b = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, b);
        gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1,-1, 3,-1, -1,3]), gl.STATIC_DRAW);
        const loc = gl.getAttribLocation(p, "a");
        gl.enableVertexAttribArray(loc);
        gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
        gl.drawArraysInstanced(gl.TRIANGLES, 0, 3, 1);
        const px = new Uint8Array(4);
        gl.readPixels(4, 4, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
        print("px=" + Array.from(px).join(","));
        print("err=" + gl.getError());
    "##.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        // Not a vertex array OBJECT until bound -- Chrome 150's `isVertexArray(createVertexArray())`
        // is false (measured; see `the_object_lifecycle_returns_real_objects`).
        "vao=true,false",
        "linked=true",
        "px=0,255,0,255",
        "err=0",
    ]);
}
