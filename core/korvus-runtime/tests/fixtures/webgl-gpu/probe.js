(function () {
  // a bot-detection script main.min.js `DM`: a float-precision / denormal fingerprint, verbatim (shaders DK, DL; canvas 256x160,
  // webgl2 first), plus the whole read column. Rows: [ctx, the 26 sampled R values, R of all 160 rows, G of all 160 rows].
  const VS = 'attribute vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }';
  const FS = 'precision highp float;\nuniform vec2 res;\nconst int MAX_LOOPS = 152;\nconst int MINEXP   = 120;\nvoid main(){\n  float y = (gl_FragCoord.y / res.y) * 32.0;\n  float x = 1.0 - (gl_FragCoord.x / res.x);\n  float fade = fract(pow(2.0, floor(y)) + x);\n  vec3 color = vec3(fade);\n\n  float xx = x;\n  int row = MINEXP + int(floor(y));\n  for (int i=0;i<MAX_LOOPS;i++) if (i < row) xx *= 0.5;\n  for (int i=0;i<MAX_LOOPS;i++) if (i < row) xx *= 2.0;\n  if (xx == 0.0) color.r = clamp(color.r + 0.5, 0.0, 1.0);\n\n  if (fract(y) >= 0.9) color = vec3(0.0);\n  gl_FragColor = vec4(color, 1.0);\n}\n';
  const out = [];
  for (const kind of ['webgl2', 'webgl']) {
    const c = document.createElement('canvas'); c.width = 256; c.height = 160;
    const t = c.getContext(kind);
    if (!t) { out.push([kind, 'none']); continue; }
    const sh = (ty, s) => { const o = t.createShader(ty); t.shaderSource(o, s); t.compileShader(o); return o; };
    const e = t.createProgram(); t.attachShader(e, sh(t.VERTEX_SHADER, VS)); t.attachShader(e, sh(t.FRAGMENT_SHADER, FS)); t.linkProgram(e);
    if (!t.getProgramParameter(e, t.LINK_STATUS)) { out.push([kind, 'link', t.getProgramInfoLog(e)]); continue; }
    const r = t.createBuffer(); t.bindBuffer(t.ARRAY_BUFFER, r); t.bufferData(t.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), t.STATIC_DRAW);
    const h = t.getAttribLocation(e, 'p');
    t.viewport(0, 0, c.width, c.height); t.useProgram(e); t.uniform2f(t.getUniformLocation(e, 'res'), c.width, c.height);
    t.bindBuffer(t.ARRAY_BUFFER, r); t.enableVertexAttribArray(h); t.vertexAttribPointer(h, 2, t.FLOAT, false, 0, 0); t.drawArrays(t.TRIANGLES, 0, 3);
    const u = new Uint8Array(1 * c.height * 4); t.readPixels(.75 * c.width | 0, 0, 1, c.height, t.RGBA, t.UNSIGNED_BYTE, u);
    const i = Math.max(1, Math.floor(c.height / 26)), o = [];
    for (let a = 0; a < 26; a++) o.push(u[4 * (a * i | 0)]);
    const R = [], G = []; for (let y = 0; y < c.height; y++) { R.push(u[4 * y]); G.push(u[4 * y + 1]); }
    out.push([kind, o.join('.'), R.join('.'), G.join('.')]);
  }
  // the device this answer is the GPU's of (`WEBGL_debug_renderer_info`)
  const g = document.createElement('canvas').getContext('webgl'), d = g.getExtension('WEBGL_debug_renderer_info');
  out.push(['device', g.getParameter(d.UNMASKED_VENDOR_WEBGL), g.getParameter(d.UNMASKED_RENDERER_WEBGL)]);
  return out;
})()
