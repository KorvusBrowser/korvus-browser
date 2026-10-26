// PURPOSE: capture the REAL render fingerprint (canvas 2D / WebGL / WebAudio + system params) of the
// target Brave 151 build, drawing a CANONICAL PROBE that the Korvus render-spine test fixtures mirror
// byte-for-byte. This JSON becomes brave-151.render.reference.json -- the bit-identical green-gate
// oracle for R2 (Skia 2D) / R3 (WebGL) / R5 (WebAudio). See docs/RENDER-SPINE.md.
//
// !!! RUN THIS WITH BRAVE FINGERPRINT PROTECTION *DISABLED* FOR THIS PAGE !!!
// We want the RAW render-core pixels (stable, permanent oracle), NOT Brave's per-session farbled
// pixels. Farbling makes the hash vary every session, so a farbled capture is useless as a fixture.
// To disable for the probe page: click the Brave Shields icon (lion) in the address bar ->
// "Fingerprinting blocked" -> set to "Allow all fingerprinting" (a.k.a. Shields DOWN). Reload, then run.
// (A later farbling-ON capture can validate R4 separately; this first one is the raw core.)
//
// HOW TO RUN (takes ~5 seconds):
//   1. Open Brave 151 (the same build the window/worker baselines came from).
//   2. Go to  https://example.com   (https, no CSP; any simple page is fine).
//   3. Brave Shields (lion icon) -> Fingerprinting -> "Allow all fingerprinting" (raw pixels). Reload.
//   4. Open DevTools (Cmd+Option+I) -> Console tab.
//   5. Paste this ENTIRE file, press Enter. It is async -- wait for the "==== COPY" banner.
//   6. Check the SANITY line: isBrave should be true and ua should contain your Brave version.
//   7. Copy the single JSON line between the ==== markers (or run  copy(__RF)  on the next line).
//   8. Paste that JSON back to Claude (or save as brave-151.render.reference.json here).
//
// If any dimension errors, its object will contain an "error" field -- paste it anyway and tell Claude.
(async () => {
  // --- helpers -------------------------------------------------------------------------------------
  // FNV-1a 32-bit over a byte view -> 8 hex chars. Stable, dependency-free, good enough to diff pixels.
  const fnv = (bytes) => {
    let h = 0x811c9dc5;
    for (let i = 0; i < bytes.length; i++) {
      h ^= bytes[i];
      h = (h + ((h << 1) + (h << 4) + (h << 7) + (h << 8) + (h << 24))) >>> 0;
    }
    return ("00000000" + h.toString(16)).slice(-8);
  };
  const bytesOf = (typed) => new Uint8Array(typed.buffer, typed.byteOffset, typed.byteLength);
  // n evenly-spaced samples of a byte view (so a diff can be localized without pasting 180 KB of raw).
  const sample = (bytes, n) => {
    const out = [];
    const step = Math.max(1, Math.floor(bytes.length / n));
    for (let i = 0; i < bytes.length; i += step) out.push(bytes[i]);
    return out;
  };

  // --- canonical CANVAS 2D probe (mirror this EXACTLY in the Rust fixture) --------------------------
  const canvas2d = (() => {
    try {
      const c = document.createElement("canvas");
      c.width = 300; c.height = 150;
      const x = c.getContext("2d");
      x.textBaseline = "alphabetic";
      x.fillStyle = "#f60"; x.fillRect(125, 1, 62, 20);
      x.fillStyle = "#069"; x.font = "11pt Arial";
      x.fillText("Korvus \u{1F600} 1.0", 2, 15);
      x.fillStyle = "rgba(102,204,0,0.7)"; x.font = "18pt Arial";
      x.fillText("Korvus \u{1F600} 1.0", 4, 45);
      x.globalCompositeOperation = "multiply";
      x.fillStyle = "rgb(255,0,255)"; x.beginPath(); x.arc(50, 50, 50, 0, Math.PI * 2, true); x.closePath(); x.fill();
      x.fillStyle = "rgb(0,255,255)"; x.beginPath(); x.arc(100, 50, 50, 0, Math.PI * 2, true); x.closePath(); x.fill();
      x.fillStyle = "rgb(255,255,0)"; x.beginPath(); x.arc(75, 100, 50, 0, Math.PI * 2, true); x.closePath(); x.fill();
      x.fillStyle = "rgb(255,0,255)";
      x.beginPath(); x.arc(75, 75, 75, 0, Math.PI * 2, true); x.arc(75, 75, 25, 0, Math.PI * 2, true);
      x.fill("evenodd");
      const dataURL = c.toDataURL();
      const id = x.getImageData(0, 0, 300, 150);
      const idb = bytesOf(id.data);
      // measureText metrics: a big text-layout fingerprint; validates R2's font stack.
      const measures = {};
      for (const [font, str] of [
        ["11pt Arial", "Korvus \u{1F600} 1.0"], ["18pt Arial", "Korvus \u{1F600} 1.0"],
        ["16px serif", "mmmmmmmmmmlli"], ["16px monospace", "\u{1F600} Ω 中"],
      ]) {
        x.font = font;
        const m = x.measureText(str);
        measures[font + "|" + str] = {
          width: m.width,
          actualBoundingBoxAscent: m.actualBoundingBoxAscent, actualBoundingBoxDescent: m.actualBoundingBoxDescent,
          actualBoundingBoxLeft: m.actualBoundingBoxLeft, actualBoundingBoxRight: m.actualBoundingBoxRight,
          fontBoundingBoxAscent: m.fontBoundingBoxAscent, fontBoundingBoxDescent: m.fontBoundingBoxDescent,
        };
      }
      return { dataURL, imageDataHash: fnv(idb), imageDataLen: idb.length, imageDataSample: sample(idb, 200), measures };
    } catch (e) { return { error: String((e && e.message) || e) }; }
  })();

  // --- canonical WEBGL probe -----------------------------------------------------------------------
  const webgl = (() => {
    try {
      const c = document.createElement("canvas");
      c.width = 256; c.height = 128;
      const gl = c.getContext("webgl", { preserveDrawingBuffer: true, antialias: true });
      if (!gl) return { error: "no webgl context" };
      const dbg = gl.getExtension("WEBGL_debug_renderer_info");
      const P = (n) => { try { const v = gl.getParameter(n); return (v && v.length !== undefined && typeof v !== "string") ? Array.from(v) : v; } catch (e) { return null; } };
      const metadata = {
        VERSION: P(gl.VERSION), SHADING_LANGUAGE_VERSION: P(gl.SHADING_LANGUAGE_VERSION),
        VENDOR: P(gl.VENDOR), RENDERER: P(gl.RENDERER),
        UNMASKED_VENDOR: dbg ? P(dbg.UNMASKED_VENDOR_WEBGL) : null,
        UNMASKED_RENDERER: dbg ? P(dbg.UNMASKED_RENDERER_WEBGL) : null,
        MAX_TEXTURE_SIZE: P(gl.MAX_TEXTURE_SIZE), MAX_VIEWPORT_DIMS: P(gl.MAX_VIEWPORT_DIMS),
        MAX_RENDERBUFFER_SIZE: P(gl.MAX_RENDERBUFFER_SIZE), MAX_VERTEX_ATTRIBS: P(gl.MAX_VERTEX_ATTRIBS),
        MAX_VERTEX_UNIFORM_VECTORS: P(gl.MAX_VERTEX_UNIFORM_VECTORS), MAX_FRAGMENT_UNIFORM_VECTORS: P(gl.MAX_FRAGMENT_UNIFORM_VECTORS),
        MAX_VARYING_VECTORS: P(gl.MAX_VARYING_VECTORS), MAX_TEXTURE_IMAGE_UNITS: P(gl.MAX_TEXTURE_IMAGE_UNITS),
        ALIASED_LINE_WIDTH_RANGE: P(gl.ALIASED_LINE_WIDTH_RANGE), ALIASED_POINT_SIZE_RANGE: P(gl.ALIASED_POINT_SIZE_RANGE),
        RED_BITS: P(gl.RED_BITS), DEPTH_BITS: P(gl.DEPTH_BITS), STENCIL_BITS: P(gl.STENCIL_BITS),
        extensions: gl.getSupportedExtensions(),
      };
      const prec = {};
      for (const [sh, sn] of [[gl.VERTEX_SHADER, "V"], [gl.FRAGMENT_SHADER, "F"]])
        for (const [pt, pn] of [[gl.HIGH_FLOAT, "HF"], [gl.MEDIUM_FLOAT, "MF"], [gl.HIGH_INT, "HI"]]) {
          const f = gl.getShaderPrecisionFormat(sh, pt);
          prec[sn + pn] = f ? [f.rangeMin, f.rangeMax, f.precision] : null;
        }
      metadata.precision = prec;
      // draw a gradient triangle-pair -> exercises interpolation + precision on the real GPU.
      const vs = gl.createShader(gl.VERTEX_SHADER);
      gl.shaderSource(vs, "attribute vec2 p; varying vec2 v; void main(){ v = p; gl_Position = vec4(p, 0.0, 1.0); }");
      gl.compileShader(vs);
      const fs = gl.createShader(gl.FRAGMENT_SHADER);
      gl.shaderSource(fs, "precision highp float; varying vec2 v; void main(){ gl_FragColor = vec4(v * 0.5 + 0.5, 0.5, 1.0); }");
      gl.compileShader(fs);
      const pr = gl.createProgram(); gl.attachShader(pr, vs); gl.attachShader(pr, fs); gl.linkProgram(pr); gl.useProgram(pr);
      const buf = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, buf);
      gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
      const loc = gl.getAttribLocation(pr, "p"); gl.enableVertexAttribArray(loc); gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
      gl.viewport(0, 0, 256, 128); gl.clearColor(0.1, 0.2, 0.3, 1.0); gl.clear(gl.COLOR_BUFFER_BIT);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      const px = new Uint8Array(256 * 128 * 4); gl.readPixels(0, 0, 256, 128, gl.RGBA, gl.UNSIGNED_BYTE, px);
      return { metadata, readPixelsHash: fnv(px), readPixelsLen: px.length, readPixelsSample: sample(px, 200), dataURL: c.toDataURL() };
    } catch (e) { return { error: String((e && e.message) || e) }; }
  })();

  // --- canonical WEBAUDIO probe (async) ------------------------------------------------------------
  const audio = await (async () => {
    try {
      const OAC = window.OfflineAudioContext || window.webkitOfflineAudioContext;
      const ctx = new OAC(1, 44100, 44100);
      const osc = ctx.createOscillator(); osc.type = "triangle"; osc.frequency.value = 10000;
      const comp = ctx.createDynamicsCompressor();
      for (const [k, v] of [["threshold", -50], ["knee", 40], ["ratio", 12], ["attack", 0], ["release", 0.25]])
        try { comp[k].value = v; } catch (e) {}
      osc.connect(comp); comp.connect(ctx.destination); osc.start(0);
      const buf = await ctx.startRendering();
      const ch = buf.getChannelData(0);
      let classicSum = 0; for (let i = 4500; i < 5000; i++) classicSum += Math.abs(ch[i]);
      const chb = bytesOf(ch);
      // THE LIVE DEVICE'S OUTPUT RATE, which the offline context above cannot report -- it renders at
      // whatever rate it was constructed with. `AudioContext.sampleRate` is the one device-varying
      // number in this family and a standard fingerprint read, and nothing recorded it: the engine
      // answered a hardcoded 48000, a persona-visible value that came from no persona.
      let liveRate = null;
      try {
        const live = new (globalThis.AudioContext || globalThis.webkitAudioContext)();
        liveRate = live.sampleRate;
        try { live.close(); } catch (e) {}
      } catch (e) {}
      return {
        sampleRate: liveRate,
        renderedHash: fnv(chb), renderedLen: ch.length, classicSum,
        sample: sample(chb, 200), slice: Array.from(ch.slice(4500, 4520)),
      };
    } catch (e) { return { error: String((e && e.message) || e) }; }
  })();

  // --- system / RenderParams the runtime extractor will also self-collect (validate against these) --
  const system = (() => {
    try {
      // font-presence probe: a candidate is installed if it changes the width of a reference string
      // vs the base generic family.
      const probe = document.createElement("span");
      probe.style.cssText = "position:absolute;left:-9999px;font-size:72px;";
      probe.textContent = "mmmmmmmmmmlli WwQq \u{1F600}";
      document.body.appendChild(probe);
      const widthIn = (family) => { probe.style.fontFamily = family; return probe.getBoundingClientRect().width; };
      const bases = { serif: widthIn("serif"), "sans-serif": widthIn("sans-serif"), monospace: widthIn("monospace") };
      const candidates = ["Arial", "Helvetica", "Helvetica Neue", "Times New Roman", "Times", "Courier",
        "Courier New", "Menlo", "Monaco", "Geneva", "Verdana", "Georgia", "Palatino", "Apple Color Emoji",
        "San Francisco", ".SF NS", "Zapfino", "Comic Sans MS", "Impact", "Arial Black"];
      const fontsPresent = candidates.filter((f) => ["serif", "sans-serif", "monospace"].some((b) => widthIn(f + "," + b) !== bases[b]));
      document.body.removeChild(probe);
      return {
        devicePixelRatio: window.devicePixelRatio,
        screen: { width: screen.width, height: screen.height, colorDepth: screen.colorDepth, pixelDepth: screen.pixelDepth },
        hardwareConcurrency: navigator.hardwareConcurrency, deviceMemory: navigator.deviceMemory,
        platform: navigator.platform, fontsPresent,
      };
    } catch (e) { return { error: String((e && e.message) || e) }; }
  })();

  // isBrave() is async; best-effort confirm we captured on Brave.
  let isBrave = false;
  try { isBrave = !!(navigator.brave && (await navigator.brave.isBrave())); } catch (e) {}

  const out = JSON.stringify({
    meta: {
      engine: (typeof globalThis.chrome !== "undefined") ? "chromium" : "korvus",
      isBrave, ua: navigator.userAgent, captured: "raw-core (fingerprint protection must be DISABLED)",
    },
    canvas2d, webgl, audio, system,
  });
  globalThis.__RF = out;
  console.log("[korvus] SANITY -> isBrave:", isBrave, "| ua:", navigator.userAgent);
  console.log("[korvus] dims -> canvas2d.err:", canvas2d.error || "none", "| webgl.err:", webgl.error || "none", "| audio.err:", audio.error || "none");
  console.log("[korvus] webgl RENDERER:", webgl.metadata && webgl.metadata.UNMASKED_RENDERER, "| fonts found:", (system.fontsPresent || []).length, "| length:", out.length);
  if (!isBrave) console.warn("[korvus] NOT DETECTED AS BRAVE -- run on Brave 151, or tell Claude what browser this is.");
  console.log("[korvus] ==== COPY THE SINGLE JSON LINE BELOW (starts with {\"meta\") ====");
  console.log(out);
  console.log("[korvus] ==== END (copy everything from { to } above) ====");
  console.log("[korvus] tip: or run   copy(__RF)   on the next console line to copy it to clipboard.");
})();
