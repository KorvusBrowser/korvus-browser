(async function () {
  // MSAA rules of the WebGL default framebuffer, as the GPU resolves them (abprog C12): where the samples sit, whether
  // the fragment shader runs once per pixel (at its centre) or per sample, and how coverage resolves. Each case draws
  // into a fresh canvas and reads the whole buffer back; a row is [case, w, h, R channel of every pixel bottom-up
  // ('.'-joined per row, rows '/'-joined; R-B for the gl_FragCoord cases)].
  const out = [];
  const VS = 'attribute vec4 p;void main(){gl_Position = p;}';
  const PX_FS = 'precision lowp float;uniform float v;void main() {vec4 c = vec4(.3, .3, .3, 1);float mx = mod(gl_FragCoord.x, 2.0);float my = mod(gl_FragCoord.y, 2.0);if (mx > 0.) c.x = .5;else c.x = 1.;if (my > 0.) c.z = .5;else c.z = 1.;c.y = c.x - c.z;gl_FragColor = c;}';
  const FLAT_FS = 'precision mediump float;void main(){gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0);}';
  // the fragment's own position: R = fract(gl_FragCoord.x), B = fract(gl_FragCoord.y) (per-sample shading shows here)
  const COORD_FS = 'precision highp float;void main(){gl_FragColor = vec4(fract(gl_FragCoord.x), 0.0, fract(gl_FragCoord.y), 1.0);}';
  function draw(w, h, aa, fs, verts, mode) {
    const c = document.createElement('canvas'); c.width = w; c.height = h;
    const gl = c.getContext('webgl', { antialias: aa, preserveDrawingBuffer: true });
    if (!gl) return null;
    const sh = (t, s) => { const o = gl.createShader(t); gl.shaderSource(o, s); gl.compileShader(o); return o; };
    const p = gl.createProgram();
    gl.attachShader(p, sh(gl.VERTEX_SHADER, VS)); gl.attachShader(p, sh(gl.FRAGMENT_SHADER, fs)); gl.linkProgram(p);
    const b = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, b);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(verts), gl.STATIC_DRAW);
    gl.clearColor(0, 0, 0, 1); gl.clear(gl.COLOR_BUFFER_BIT); gl.viewport(0, 0, w, h);
    gl.useProgram(p); const loc = gl.getAttribLocation(p, 'p'); gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0); gl.drawArrays(mode === 'strip' ? gl.TRIANGLE_STRIP : gl.TRIANGLES, 0, verts.length / 2);
    const px = new Uint8Array(w * h * 4); gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, px);
    const rows = [];
    for (let y = 0; y < h; y++) { const r = []; for (let x = 0; x < w; x++) r.push(px[(y * w + x) * 4] + (fs === COORD_FS ? '-' + px[(y * w + x) * 4 + 2] : '')); rows.push(r.join('.')); }
    return { gl, rows: rows.join('/') };
  }
  const cases = [
    ['px-27x57', 27, 57, true, PX_FS, [-1, 1, 1, 1, 0, -1, -1, 1], 'strip'],
    ['px-16x9', 16, 9, true, PX_FS, [-1, 1, 1, 1, 0, -1, -1, 1], 'strip'],
    ['px-27x57-noaa', 27, 57, false, PX_FS, [-1, 1, 1, 1, 0, -1, -1, 1], 'strip'],
    // asymmetric slivers: a shallow edge and a steep edge, both directions -- each pixel's coverage count is the
    // number of sample positions under the edge, which pins the positions
    ['flat-shallow', 8, 8, true, FLAT_FS, [-1, -1, 1, -0.9, -1, 0.95], 'tri'],
    ['flat-steep', 8, 8, true, FLAT_FS, [-1, -1, -0.9, 1, 0.95, -1], 'tri'],
    ['flat-diag', 8, 8, true, FLAT_FS, [-1, -1, 1, -1, -1, 1], 'tri'],
    ['flat-diag2', 8, 8, true, FLAT_FS, [1, -1, 1, 1, -1, 1], 'tri'],
    ['flat-odd', 9, 7, true, FLAT_FS, [-0.83, -0.71, 0.77, -0.33, -0.12, 0.91], 'tri'],
    ['coord-odd', 9, 7, true, COORD_FS, [-0.83, -0.71, 0.77, -0.33, -0.12, 0.91], 'tri'],
    ['coord-noaa', 9, 7, false, COORD_FS, [-0.83, -0.71, 0.77, -0.33, -0.12, 0.91], 'tri'],
  ];
  for (const [name, w, h, aa, fs, v, mode] of cases) {
    const r = draw(w, h, aa, fs, v, mode);
    out.push(r ? [name, w, h, r.rows] : [name, 'no-webgl']);
  }
  // A RESIZE of the canvas the context draws into (a bot-detection script draws each frame at a new size): a new size
  // reallocates the drawing buffer cleared; the size it already has keeps the pixels; a draw after the
  // resize lands in the new buffer.
  const reread = (gl, w, h) => {
    const px = new Uint8Array(w * h * 4); gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, px);
    const rows = [];
    for (let y = 0; y < h; y++) { const r = []; for (let x = 0; x < w; x++) r.push(px[(y * w + x) * 4] + '-' + px[(y * w + x) * 4 + 3]); rows.push(r.join('.')); }
    return rows.join('/');
  };
  const quad = [-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1];
  let r = draw(8, 8, true, FLAT_FS, quad, 'tri');
  if (r) {
    const c = r.gl.canvas;
    c.width = 8;
    out.push(['resize-same', 8, 8, reread(r.gl, 8, 8)]);
    c.width = 4; c.height = 3;
    out.push(['resize-new', 4, 3, reread(r.gl, 4, 3)]);
    r.gl.drawArrays(r.gl.TRIANGLES, 0, 3);
    out.push(['resize-redraw', 4, 3, reread(r.gl, 4, 3), r.gl.drawingBufferWidth + 'x' + r.gl.drawingBufferHeight]);
  }
  return out;
})()
