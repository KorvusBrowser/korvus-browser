// PARITY P10d -- the canvas image, tainting, WebGL extension and window-accessor probe.
//
// Evaluated UNMODIFIED by `tests/canvas_draw_image_live.rs` and by stock Chrome 150.0.7871.46 over
// CDP, against a page on origin `same` (serving this directory) with a second origin `xo` serving it
// too, where every path under `/cors/` carries `Access-Control-Allow-Origin: *` and `/missing.png` and
// `/garbage/*` are a 404 and a 200 of non-image bytes. `canvas-probe-server.py` is that server.
// `section` picks what to print: "canvas", "webgl", "globals", or "gpu-ext" (the extensions only a
// GPU-backed Chrome exposes, recorded with `--use-angle=gl-egl` into their own file).
async (same, xo, section) => {
  const out = [];
  const log = (k, v) => out.push(k + ' => ' + v);
  const T = async (k, f) => { try { log(k, await f()); } catch (e) { log(k, 'THROW ' + e.name + ': ' + e.message); } };
  const load = (src, co) => new Promise((r) => {
    const i = new Image();
    if (co !== undefined) i.crossOrigin = co;
    i.onload = () => r(i); i.onerror = () => r(i);
    i.src = src;
    setTimeout(() => r(i), 4000);
  });
  const px = (ctx, x, y) => { try { return Array.from(ctx.getImageData(x, y, 1, 1).data).join(','); } catch (e) { return 'THROW ' + e.name + ': ' + e.message; } };
  const mk = (w = 20, h = 20) => { const c = document.createElement('canvas'); c.width = w; c.height = h; return [c, c.getContext('2d')]; };
  const quad = (x) => [px(x, 2, 2), px(x, 15, 2), px(x, 2, 15), px(x, 15, 15)].join(' | ');
  const cib = async (...a) => { try { const b = await createImageBitmap(...a); return 'bitmap ' + b.width + 'x' + b.height; } catch (e) { return 'REJECT ' + e.name + ': ' + e.message; } };

  if (section === 'globals') {
    for (const n of ['crypto', 'indexedDB', 'caches', 'createImageBitmap']) {
      const d = Object.getOwnPropertyDescriptor(window, n);
      if (!d) { log('desc ' + n, 'absent'); continue; }
      log('desc ' + n, ('value' in d ? 'data w=' + d.writable + ' ' + typeof d.value + ' ' + (d.value && d.value.name) + '/' + (d.value && d.value.length) : 'accessor get=' + d.get.name + '/' + d.get.length + ' set=' + d.set) + ' e=' + d.enumerable + ' c=' + d.configurable);
    }
    log('crypto same object', window.crypto === crypto && crypto === Object.getOwnPropertyDescriptor(window, 'crypto').get.call(window));
    log('indexedDB same object', indexedDB === indexedDB && Object.prototype.toString.call(indexedDB));
    return out.join('\n');
  }

  if (section === 'canvas') {
    let i, c, x;
    i = await load(same + 'red.png'); [c, x] = mk(); x.drawImage(i, 0, 0); log('red.png', px(x, 5, 5));
    for (const f of ['quad.png', 'quad.jpg', 'quad.gif', 'quad.bmp']) {
      i = await load(same + f); [c, x] = mk(); x.drawImage(i, 0, 0); log(f, quad(x));
    }
    i = await load(same + 'quad.png');
    [c, x] = mk(40, 40); x.imageSmoothingEnabled = false; x.drawImage(i, 0, 0, 40, 40); log('5-arg scale x2, no smoothing', [px(x, 5, 5), px(x, 25, 5), px(x, 5, 25), px(x, 25, 25), px(x, 19, 19), px(x, 20, 20)].join(' | '));
    [c, x] = mk(); x.drawImage(i, 10, 0, 10, 10, 0, 0, 20, 20); log('9-arg one quadrant scaled', [px(x, 1, 1), px(x, 10, 10)].join(' | '));
    [c, x] = mk(); x.drawImage(i, 5, 5, 10, 10, 0, 0, 10, 10); log('9-arg centre', [px(x, 2, 2), px(x, 7, 2), px(x, 2, 7), px(x, 7, 7), px(x, 15, 15)].join(' | '));
    [c, x] = mk(); x.drawImage(i, -10, -10, 20, 20, 0, 0, 20, 20); log('9-arg source partly outside', [px(x, 2, 2), px(x, 12, 12), px(x, 15, 15)].join(' | '));
    [c, x] = mk(); x.drawImage(i, 20, 0, -10, 10, 0, 0, 10, 10); log('9-arg negative sw', [px(x, 2, 2), px(x, 12, 2)].join(' | '));
    [c, x] = mk(); x.drawImage(i, 0, 0, 20, 20, 20, 20, -20, -20); log('9-arg negative dw', [px(x, 2, 2), px(x, 15, 15)].join(' | '));
    [c, x] = mk(); x.globalAlpha = 0.5; x.drawImage(i, 0, 0); log('globalAlpha 0.5', [px(x, 2, 2), px(x, 15, 15)].join(' | '));
    [c, x] = mk(); x.translate(10, 0); x.drawImage(i, 0, 0); log('translate', [px(x, 2, 2), px(x, 12, 2)].join(' | '));
    [c, x] = mk(); x.drawImage(i, 0, 0, NaN, 10); log('NaN width', px(x, 2, 2));
    [c, x] = mk(); x.drawImage(i, 10, 0, 10, 10, 0, 0, 20, 20); log('9-arg one quadrant scaled, edge', [px(x, 19, 19), px(x, 10, 19)].join(' | '));
    [c, x] = mk();
    for (const a of [[], [i], [i, 0], [i, 0, 0, 1], [i, 0, 0, 1, 1, 1], [i, 0, 0, 1, 1, 1, 1], [i, 0, 0, 1, 1, 1, 1, 1], [{}, 0, 0], [null, 0, 0], [undefined, 0, 0], ['x', 0, 0], [new ImageData(1, 1), 0, 0]]) {
      const k = 'drawImage args=' + a.length + ' a0=' + (a[0] === i ? 'img' : Object.prototype.toString.call(a[0]));
      try { x.drawImage(...a); log(k, 'ok'); } catch (e) { log(k, 'THROW ' + e.name + ': ' + e.message); }
    }
    await T('img with no src', () => { const [, y] = mk(); y.drawImage(new Image(), 0, 0); return 'ok ' + px(y, 2, 2); });
    await T('img 404', async () => { const u = await load(same + 'missing.png'); const [, y] = mk(); y.drawImage(u, 0, 0); return 'ok'; });
    await T('img not an image', async () => { const u = await load(same + 'garbage/quad.png'); const [, y] = mk(); y.drawImage(u, 0, 0); return 'ok'; });
    await T('canvas source', () => { const [s, sx] = mk(); sx.fillStyle = '#00ff00'; sx.fillRect(0, 0, 10, 10); const [, y] = mk(); y.drawImage(s, 0, 0); return px(y, 2, 2) + ' | ' + px(y, 15, 15); });
    await T('canvas source 0 wide', () => { const s = document.createElement('canvas'); s.width = 0; const [, y] = mk(); y.drawImage(s, 0, 0); return 'ok'; });
    await T('canvas source with no context', () => { const [, y] = mk(); y.drawImage(document.createElement('canvas'), 0, 0); return 'ok ' + px(y, 2, 2); });
    await T('OffscreenCanvas source', () => { const o = new OffscreenCanvas(20, 20); const ox = o.getContext('2d'); ox.fillStyle = '#0000ff'; ox.fillRect(0, 0, 20, 20); const [, y] = mk(); y.drawImage(o, 0, 0); return px(y, 5, 5); });
    await T('OffscreenCanvas ctx drawImage(img)', () => { const o = new OffscreenCanvas(20, 20); const ox = o.getContext('2d'); ox.drawImage(i, 0, 0); return Array.from(ox.getImageData(15, 2, 1, 1).data).join(','); });
    await T('OffscreenCanvas ctx drawImage({})', () => { const ox = new OffscreenCanvas(2, 2).getContext('2d'); ox.drawImage({}, 0, 0); return 'ok'; });
    await T('video never played', () => { const [, y] = mk(); y.drawImage(document.createElement('video'), 0, 0); return 'ok ' + px(y, 2, 2); });
    await T('createImageBitmap(img)', async () => { const b = await createImageBitmap(i); const [, y] = mk(); y.drawImage(b, 0, 0); const r = b.width + 'x' + b.height + ' ' + px(y, 2, 2) + ' | ' + px(y, 15, 15); b.close(); try { y.drawImage(b, 0, 0); return r + ' closed:ok'; } catch (e) { return r + ' closed:THROW ' + e.name + ': ' + e.message; } });
    await T('createImageBitmap crop', async () => { const b = await createImageBitmap(i, 10, 0, 10, 10); const [, y] = mk(); y.drawImage(b, 0, 0); return b.width + ' ' + px(y, 2, 2) + ' | ' + px(y, 15, 15); });
    await T('createImageBitmap crop negative', async () => { const b = await createImageBitmap(i, 20, 0, -10, 10); const [, y] = mk(); y.drawImage(b, 0, 0); return b.width + 'x' + b.height + ' ' + px(y, 2, 2); });
    await T('createImageBitmap crop beyond', async () => { const b = await createImageBitmap(i, 15, 15, 10, 10); const [, y] = mk(); y.drawImage(b, 0, 0); return b.width + 'x' + b.height + ' ' + px(y, 2, 2) + ' | ' + px(y, 7, 7); });
    await T('createImageBitmap(ImageData)', async () => { const b = await createImageBitmap(new ImageData(new Uint8ClampedArray([1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]), 2, 2)); const [, y] = mk(); y.drawImage(b, 0, 0); return px(y, 0, 0) + ' | ' + px(y, 1, 1); });
    await T('createImageBitmap(canvas)', async () => { const [s, sx] = mk(); sx.fillStyle = '#123456'; sx.fillRect(0, 0, 20, 20); const b = await createImageBitmap(s); const [, y] = mk(); y.drawImage(b, 0, 0); return px(y, 3, 3); });
    await T('createImageBitmap(blob)', async () => { const bl = await (await fetch(same + 'quad.png')).blob(); const b = await createImageBitmap(new Blob([bl], { type: 'text/plain' })); const [, y] = mk(); y.drawImage(b, 0, 0); return b.width + ' ' + px(y, 2, 2) + ' | ' + px(y, 15, 15); });
    await T('createImageBitmap resize pixelated', async () => { const b = await createImageBitmap(i, { resizeWidth: 40, resizeHeight: 40, resizeQuality: 'pixelated' }); const [, y] = mk(40, 40); y.drawImage(b, 0, 0); return b.width + 'x' + b.height + ' ' + px(y, 19, 19) + ' | ' + px(y, 20, 20); });
    await T('createImageBitmap resizeWidth only', async () => { const b = await createImageBitmap(i, { resizeWidth: 10 }); return b.width + 'x' + b.height; });
    await T('createImageBitmap flipY', async () => { const b = await createImageBitmap(i, { imageOrientation: 'flipY' }); const [, y] = mk(); y.drawImage(b, 0, 0); return [px(y, 2, 2), px(y, 15, 2), px(y, 2, 15)].join(' | '); });
    await T('createImageBitmap errors', async () => [await cib(), await cib({}), await cib(i, 0), await cib(i, 0, 0), await cib(i, 0, 0, 0, 5), await cib(i, 0, 0, 5, 0), await cib(new Image()), await cib(await load(same + 'missing.png')), await cib((() => { const z = document.createElement('canvas'); z.width = 0; return z; })()), await cib(document.createElement('video')), await cib(new Blob(['nope'])), await cib(i, { resizeWidth: 0 }), await cib(i, { imageOrientation: 'sideways' })].join(' ## '));
    await T('ImageBitmap shape', async () => { const b = await createImageBitmap(i); return [Object.prototype.toString.call(b), Object.getOwnPropertyNames(b).length, b.close(), b.width, b.height].join(' '); });
    await T('ImageBitmap width on a foreign receiver', () => Object.getOwnPropertyDescriptor(ImageBitmap.prototype, 'width').get.call({}));
    await T('transferToImageBitmap', () => { const o = new OffscreenCanvas(4, 4); const ox = o.getContext('2d'); ox.fillStyle = '#ff00ff'; ox.fillRect(0, 0, 4, 4); const b = o.transferToImageBitmap(); const [, y] = mk(); y.drawImage(b, 0, 0); ox.fillRect(0, 0, 1, 1); return [Object.prototype.toString.call(b), b.width, px(y, 1, 1), Array.from(ox.getImageData(2, 2, 1, 1).data).join(','), Array.from(ox.getImageData(0, 0, 1, 1).data).join(',')].join(' '); });
    await T('transferToImageBitmap with no context', () => { new OffscreenCanvas(4, 4).transferToImageBitmap(); return 'ok'; });
    await T('convertToBlob', async () => { const o = new OffscreenCanvas(4, 4); o.getContext('2d').fillRect(0, 0, 4, 4); return [(await o.convertToBlob()).type, (await o.convertToBlob({ type: 'image/jpeg' })).type, (await o.convertToBlob({ type: 'image/bogus' })).type].join(' '); });
    await T('convertToBlob with no context', async () => { await new OffscreenCanvas(4, 4).convertToBlob(); return 'ok'; });
    await T('createPattern sources', async () => { const [, y] = mk(); const r = []; for (const v of [{}, new Image(), await load(same + 'missing.png'), (() => { const z = document.createElement('canvas'); z.width = 0; return z; })()]) { try { r.push(String(y.createPattern(v, 'repeat'))); } catch (e) { r.push(e.name + ': ' + e.message); } } return r.join(' ## '); });
    await T('createPattern(img) fills', () => { const [, y] = mk(40, 40); y.fillStyle = y.createPattern(i, 'repeat'); y.fillRect(0, 0, 40, 40); return [px(y, 2, 2), px(y, 22, 2), px(y, 35, 35)].join(' | '); });
    await T('width reset resets state', () => { const [s, y] = mk(); y.fillStyle = '#ff0000'; y.fillRect(0, 0, 5, 5); s.width = 20; return y.fillStyle + ' ' + px(y, 1, 1); });

    // ── tainting ──
    let xi = await load(xo + 'quad.png'); [c, x] = mk(); x.drawImage(xi, 0, 0);
    log('cross-origin img drawn: getImageData', px(x, 2, 2));
    await T('cross-origin: toDataURL', () => { c.toDataURL(); return 'ok'; });
    await T('cross-origin: toDataURL jpeg', () => { c.toDataURL('image/jpeg'); return 'ok'; });
    await T('cross-origin: toBlob', () => new Promise((r) => { c.toBlob((b) => r('callback ' + (b && b.size > 0))); }));
    await T('cross-origin: isPointInPath', () => { x.rect(0, 0, 5, 5); return x.isPointInPath(1, 1); });
    x.clearRect(0, 0, 20, 20); log('cross-origin: after clearRect', px(x, 2, 2));
    x.reset(); log('cross-origin: after reset()', px(x, 2, 2));
    c.width = 20; log('cross-origin: after width = width', px(x, 2, 2));
    x.drawImage(xi, 0, 0); c.setAttribute('width', '20'); log('cross-origin: after setAttribute(width)', px(x, 2, 2));
    await T('cross-origin: zero-size destination', () => { const [, y] = mk(); y.drawImage(xi, 0, 0, 0, 0); return px(y, 2, 2); });
    await T('cross-origin: zero-size source', () => { const [, y] = mk(); y.drawImage(xi, 0, 0, 0, 5, 0, 0, 5, 5); return px(y, 2, 2); });
    await T('cross-origin: drawn off the bitmap', () => { const [, y] = mk(); y.drawImage(xi, 100, 100); return px(y, 2, 2); });
    await T('tainted canvas as a source', () => { const [s, sx] = mk(); sx.drawImage(xi, 0, 0); const [, y] = mk(); y.drawImage(s, 0, 0); return px(y, 2, 2); });
    await T('cross-origin pattern: created / assigned / filled', () => { const [, y] = mk(); const p = y.createPattern(xi, 'repeat'); const a = px(y, 2, 2); y.fillStyle = p; const b = px(y, 2, 2); y.fillRect(0, 0, 1, 1); return a + ' ## ' + b + ' ## ' + px(y, 2, 2); });
    await T('cross-origin pattern as strokeStyle', () => { const [, y] = mk(); y.strokeStyle = y.createPattern(xi, 'repeat'); return px(y, 2, 2); });
    await T('bitmap of a cross-origin img', async () => { const b = await createImageBitmap(xi); const [, y] = mk(); y.drawImage(b, 0, 0); return b.width + ' ' + px(y, 2, 2); });
    await T('OffscreenCanvas: getImageData after cross-origin draw', () => { const ox = new OffscreenCanvas(20, 20).getContext('2d'); ox.drawImage(xi, 0, 0); return Array.from(ox.getImageData(0, 0, 1, 1).data).join(','); });
    await T('OffscreenCanvas: convertToBlob after cross-origin draw', async () => { const o = new OffscreenCanvas(20, 20); o.getContext('2d').drawImage(xi, 0, 0); await o.convertToBlob(); return 'ok'; });
    await T('OffscreenCanvas: width reset clears the taint', () => { const o = new OffscreenCanvas(4, 4); const ox = o.getContext('2d'); ox.drawImage(xi, 0, 0); o.width = 4; return Array.from(ox.getImageData(0, 0, 1, 1).data).join(','); });
    await T('texImage2D of tainted sources', async () => { const g = document.createElement('canvas').getContext('webgl2'); g.bindTexture(g.TEXTURE_2D, g.createTexture()); const r = []; const [s, sx] = mk(); sx.drawImage(xi, 0, 0); const b = await createImageBitmap(xi); for (const f of [() => g.texImage2D(g.TEXTURE_2D, 0, g.RGBA, g.RGBA, g.UNSIGNED_BYTE, xi), () => g.texSubImage2D(g.TEXTURE_2D, 0, 0, 0, g.RGBA, g.UNSIGNED_BYTE, xi), () => g.texImage2D(g.TEXTURE_2D, 0, g.RGBA, g.RGBA, g.UNSIGNED_BYTE, s), () => g.texImage2D(g.TEXTURE_2D, 0, g.RGBA, g.RGBA, g.UNSIGNED_BYTE, b)]) { try { f(); r.push('ok ' + g.getError()); } catch (e) { r.push(e.name + ': ' + e.message); } } return r.join(' ## '); });
    xi = await load(xo + 'cors/quad.png', 'anonymous'); [c, x] = mk(); x.drawImage(xi, 0, 0); log('crossorigin=anonymous + ACAO *', px(x, 2, 2));
    xi = await load(xo + 'quad.png', 'anonymous'); await T('crossorigin=anonymous, no ACAO', () => { const [, y] = mk(); y.drawImage(xi, 0, 0); return 'ok ' + px(y, 2, 2); });
    xi = await load(xo + 'cors/quad.png', 'use-credentials'); await T('crossorigin=use-credentials, ACAO *', () => { const [, y] = mk(); y.drawImage(xi, 0, 0); return 'ok ' + px(y, 2, 2); });
    xi = await load(xo + 'cors/quad.png'); [c, x] = mk(); x.drawImage(xi, 0, 0); log('no crossorigin, ACAO *', px(x, 2, 2));
    xi = await load(same + 'quad.png', 'anonymous'); [c, x] = mk(); x.drawImage(xi, 0, 0); log('same-origin, crossorigin=anonymous', px(x, 2, 2));
    await T('texImage2D(img) uploads its pixels', () => { const cv = document.createElement('canvas'); cv.width = 20; cv.height = 20; const g = cv.getContext('webgl', { premultipliedAlpha: false }); const sh = (ty, src) => { const s = g.createShader(ty); g.shaderSource(s, src); g.compileShader(s); return s; }; const p = g.createProgram(); g.attachShader(p, sh(g.VERTEX_SHADER, 'attribute vec2 a; varying vec2 v; void main(){ v = (a + 1.0) * 0.5; gl_Position = vec4(a, 0.0, 1.0); }')); g.attachShader(p, sh(g.FRAGMENT_SHADER, 'precision mediump float; varying vec2 v; uniform sampler2D s; void main(){ gl_FragColor = texture2D(s, v); }')); g.linkProgram(p); g.useProgram(p); g.bindBuffer(g.ARRAY_BUFFER, g.createBuffer()); g.bufferData(g.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]), g.STATIC_DRAW); const loc = g.getAttribLocation(p, 'a'); g.enableVertexAttribArray(loc); g.vertexAttribPointer(loc, 2, g.FLOAT, false, 0, 0); g.bindTexture(g.TEXTURE_2D, g.createTexture()); for (const [k, v] of [[g.TEXTURE_MIN_FILTER, g.NEAREST], [g.TEXTURE_MAG_FILTER, g.NEAREST], [g.TEXTURE_WRAP_S, g.CLAMP_TO_EDGE], [g.TEXTURE_WRAP_T, g.CLAMP_TO_EDGE]]) g.texParameteri(g.TEXTURE_2D, k, v); g.texImage2D(g.TEXTURE_2D, 0, g.RGBA, g.RGBA, g.UNSIGNED_BYTE, i); g.drawArrays(g.TRIANGLES, 0, 6); const rd = (x, y) => { const a = new Uint8Array(4); g.readPixels(x, y, 1, 1, g.RGBA, g.UNSIGNED_BYTE, a); return a.join(','); }; return [rd(2, 2), rd(15, 2), rd(2, 15), rd(15, 15)].join(' | ') + ' err=' + g.getError(); });
    return out.join('\n');
  }

  const shape = (g, n) => {
    const e = g.getExtension(n);
    if (!e) return n + '=null';
    const p = Object.getPrototypeOf(e);
    const parts = Object.getOwnPropertyNames(p).map((k) => {
      const d = Object.getOwnPropertyDescriptor(p, k);
      if (typeof d.value === 'function') return k + '()' + d.value.length + (d.enumerable ? 'e' : '') + (d.writable ? 'w' : '') + (d.configurable ? 'c' : '');
      if ('value' in d) return k + '=' + d.value + (d.enumerable ? 'e' : '') + (d.writable ? 'w' : '') + (d.configurable ? 'c' : '');
      return k + '[acc]';
    });
    const tag = Object.getOwnPropertyDescriptor(p, Symbol.toStringTag);
    return n + '|' + Object.prototype.toString.call(e) + '|tag=' + (tag && (tag.value + (tag.writable ? 'w' : '') + (tag.enumerable ? 'e' : '') + (tag.configurable ? 'c' : ''))) + '|parent=' + (Object.getPrototypeOf(p) === Object.prototype) + '|same=' + (e === g.getExtension(n)) + '|' + parts.join(',');
  };

  if (section === 'gpu-ext') {
    for (const v of ['webgl', 'webgl2']) {
      const g = document.createElement('canvas').getContext(v);
      const names = v === 'webgl' ? ['EXT_disjoint_timer_query', 'KHR_parallel_shader_compile', 'WEBGL_blend_func_extended'] : ['EXT_disjoint_timer_query_webgl2', 'EXT_render_snorm', 'EXT_texture_norm16', 'KHR_parallel_shader_compile', 'WEBGL_blend_func_extended'];
      for (const n of names) log(v, shape(g, n));
    }
    return out.join('\n');
  }

  if (section === 'webgl') {
    const common = {
      webgl: ['ANGLE_instanced_arrays', 'EXT_blend_minmax', 'EXT_clip_control', 'EXT_color_buffer_half_float', 'EXT_depth_clamp', 'EXT_float_blend', 'EXT_frag_depth', 'EXT_polygon_offset_clamp', 'EXT_shader_texture_lod', 'EXT_texture_compression_bptc', 'EXT_texture_compression_rgtc', 'EXT_texture_filter_anisotropic', 'EXT_texture_mirror_clamp_to_edge', 'EXT_sRGB', 'OES_element_index_uint', 'OES_fbo_render_mipmap', 'OES_standard_derivatives', 'OES_texture_float', 'OES_texture_float_linear', 'OES_texture_half_float', 'OES_texture_half_float_linear', 'OES_vertex_array_object', 'WEBGL_color_buffer_float', 'WEBGL_compressed_texture_s3tc', 'WEBGL_compressed_texture_s3tc_srgb', 'WEBGL_debug_renderer_info', 'WEBGL_debug_shaders', 'WEBGL_depth_texture', 'WEBGL_draw_buffers', 'WEBGL_lose_context', 'WEBGL_multi_draw', 'WEBGL_polygon_mode'],
      webgl2: ['EXT_clip_control', 'EXT_color_buffer_float', 'EXT_color_buffer_half_float', 'EXT_conservative_depth', 'EXT_depth_clamp', 'EXT_float_blend', 'EXT_polygon_offset_clamp', 'EXT_texture_compression_bptc', 'EXT_texture_compression_rgtc', 'EXT_texture_filter_anisotropic', 'EXT_texture_mirror_clamp_to_edge', 'NV_shader_noperspective_interpolation', 'OES_draw_buffers_indexed', 'OES_sample_variables', 'OES_shader_multisample_interpolation', 'OES_texture_float_linear', 'OVR_multiview2', 'WEBGL_clip_cull_distance', 'WEBGL_compressed_texture_s3tc', 'WEBGL_compressed_texture_s3tc_srgb', 'WEBGL_debug_renderer_info', 'WEBGL_debug_shaders', 'WEBGL_lose_context', 'WEBGL_multi_draw', 'WEBGL_polygon_mode', 'WEBGL_stencil_texturing'],
    };
    for (const v of ['webgl', 'webgl2']) {
      const g = document.createElement('canvas').getContext(v);
      for (const n of common[v]) log(v, shape(g, n));
      log(v + ' names that are no extension', [g.getExtension('FOO_bar'), g.getExtension('MOZ_WEBGL_lose_context'), Object.prototype.toString.call(g.getExtension('webgl_LOSE_context'))].join(' '));
      log(v + ' no global names the interfaces', [typeof WebGLLoseContext, typeof ANGLEInstancedArrays, typeof WebGLVertexArrayObjectOES].join(' '));
    }
    await T('loseContext', async () => {
      const cv = document.createElement('canvas'); const g = cv.getContext('webgl'); const e = g.getExtension('WEBGL_lose_context');
      const prog = g.createProgram(); const sh = g.createShader(g.VERTEX_SHADER); const buf = g.createBuffer();
      const fired = []; cv.addEventListener('webglcontextlost', (ev) => fired.push('lost:' + ev.constructor.name + ':' + ev.cancelable + ':' + ev.isTrusted)); cv.addEventListener('webglcontextrestored', () => fired.push('restored'));
      const r = ['fn=' + e.loseContext.name + '/' + e.loseContext.length];
      e.loseContext();
      const q = (k, f) => { try { const v = f(); r.push(k + ':' + (v === null ? 'null' : typeof v === 'object' ? Object.prototype.toString.call(v) : String(v))); } catch (er) { r.push(k + ':THROW ' + er.name); } };
      q('fired', () => fired.join(';') || 'none'); q('getError', () => g.getError()); q('getError', () => g.getError()); q('isContextLost', () => g.isContextLost());
      q('getParameter', () => g.getParameter(g.MAX_TEXTURE_SIZE)); q('getShaderPrecisionFormat', () => g.getShaderPrecisionFormat(g.VERTEX_SHADER, g.HIGH_FLOAT));
      q('createBuffer', () => g.createBuffer()); q('isBuffer', () => g.isBuffer(buf)); q('isProgram', () => g.isProgram(prog)); q('isEnabled', () => g.isEnabled(g.BLEND));
      q('getAttribLocation', () => g.getAttribLocation(prog, 'a')); q('getUniformLocation', () => g.getUniformLocation(prog, 'u')); q('checkFramebufferStatus', () => g.checkFramebufferStatus(g.FRAMEBUFFER));
      q('getProgramParameter', () => g.getProgramParameter(prog, g.LINK_STATUS)); q('getShaderInfoLog', () => g.getShaderInfoLog(sh)); q('getVertexAttribOffset', () => g.getVertexAttribOffset(0, g.VERTEX_ATTRIB_ARRAY_POINTER));
      q('getContextAttributes', () => g.getContextAttributes()); q('getSupportedExtensions', () => g.getSupportedExtensions()); q('getExtension', () => g.getExtension('OES_vertex_array_object'));
      q('drawingBufferWidth', () => g.drawingBufferWidth); q('readPixels', () => { const a = new Uint8Array(4).fill(7); g.readPixels(0, 0, 1, 1, g.RGBA, g.UNSIGNED_BYTE, a); return a.join(','); });
      q('loseContext again', () => e.loseContext()); q('getError', () => g.getError());
      await new Promise((res) => setTimeout(res, 50));
      q('fired', () => fired.join(';')); q('getError', () => g.getError());
      e.restoreContext(); await new Promise((res) => setTimeout(res, 50));
      q('restore refused: isContextLost', () => g.isContextLost()); q('getError', () => g.getError()); q('fired', () => fired.join(';'));
      return r.join(' ; ');
    });
    await T('lost event is a task', async () => { const cv = document.createElement('canvas'); const g = cv.getContext('webgl'); const seq = []; cv.addEventListener('webglcontextlost', () => seq.push('listener')); g.getExtension('WEBGL_lose_context').loseContext(); seq.push('sync'); await Promise.resolve(); seq.push('microtask'); await new Promise((r) => setTimeout(r, 30)); return seq.join(','); });
    await T('restoreContext after preventDefault', async () => {
      const cv = document.createElement('canvas'); const g = cv.getContext('webgl'); const e = g.getExtension('WEBGL_lose_context'); const tex = g.createTexture(); const seq = [];
      cv.addEventListener('webglcontextlost', (ev) => { ev.preventDefault(); seq.push('lost'); }); cv.addEventListener('webglcontextrestored', (ev) => seq.push('restored ' + g.isContextLost() + ' ' + ev.cancelable));
      g.clearColor(1, 0, 0, 1); e.loseContext(); await new Promise((r) => setTimeout(r, 20));
      e.restoreContext(); seq.push('restore sync lost=' + g.isContextLost() + ' err=' + g.getError());
      await new Promise((r) => setTimeout(r, 50));
      seq.push('isTexture(old)=' + g.isTexture(tex) + ' err=' + g.getError() + ' clearColor=' + Array.from(g.getParameter(g.COLOR_CLEAR_VALUE)).join(',') + ' sameExt=' + (e === g.getExtension('WEBGL_lose_context')));
      return seq.join(' ; ');
    });
    await T('restoreContext when not lost', () => { const g = document.createElement('canvas').getContext('webgl'); g.getExtension('WEBGL_lose_context').restoreContext(); return g.getError(); });
    await T('extension methods on foreign receivers', () => { const g = document.createElement('canvas').getContext('webgl'); const e = g.getExtension('WEBGL_lose_context'); const r = []; for (const t of [{}, Object.create(Object.getPrototypeOf(e))]) { try { e.loseContext.call(t); r.push('ok'); } catch (er) { r.push(er.name + ': ' + er.message); } } return r.join(' ## ') + ' ## lost=' + g.isContextLost(); });
    await T('OES_vertex_array_object', () => { const g = document.createElement('canvas').getContext('webgl'); const v = g.getExtension('OES_vertex_array_object'); const a = v.createVertexArrayOES(); const r = [Object.prototype.toString.call(a), Object.getOwnPropertyNames(Object.getPrototypeOf(a)).length, v.isVertexArrayOES(a)]; v.bindVertexArrayOES(a); r.push(v.isVertexArrayOES(a)); v.bindVertexArrayOES(); v.deleteVertexArrayOES(a); r.push(v.isVertexArrayOES(a), v.isVertexArrayOES()); return r.join(' '); });
    await T('ANGLE_instanced_arrays arity', () => { const g = document.createElement('canvas').getContext('webgl'); g.getExtension('ANGLE_instanced_arrays').vertexAttribDivisorANGLE(0); return 'ok'; });
    await T('WEBGL_debug_shaders on an uncompiled shader', () => { const g = document.createElement('canvas').getContext('webgl'); return JSON.stringify(g.getExtension('WEBGL_debug_shaders').getTranslatedShaderSource(g.createShader(g.VERTEX_SHADER))); });
    await T('WEBGL_multi_draw drawcount past the lists', () => { const g = document.createElement('canvas').getContext('webgl'); const m = g.getExtension('WEBGL_multi_draw'); m.multiDrawArraysWEBGL(g.TRIANGLES, [0], 0, [3], 0, 5); return g.getError(); });
    for (const [n, pn, ext] of [['UNMASKED_VENDOR_WEBGL', 37445, 'WEBGL_debug_renderer_info'], ['MAX_TEXTURE_MAX_ANISOTROPY_EXT', 34047, 'EXT_texture_filter_anisotropic'], ['MAX_DRAW_BUFFERS_WEBGL', 34852, 'WEBGL_draw_buffers'], ['FRAGMENT_SHADER_DERIVATIVE_HINT_OES', 35723, 'OES_standard_derivatives'], ['CLIP_ORIGIN_EXT', 37724, 'EXT_clip_control'], ['POLYGON_MODE_WEBGL', 2880, 'WEBGL_polygon_mode'], ['POLYGON_OFFSET_CLAMP_EXT', 36379, 'EXT_polygon_offset_clamp'], ['DEPTH_CLAMP_EXT', 34383, 'EXT_depth_clamp']]) {
      for (const v of ['webgl', 'webgl2']) {
        await T('gate ' + v + ' ' + n, () => { const g = document.createElement('canvas').getContext(v); const p = g.getParameter(pn); const before = (p === null ? 'null' : typeof p) + ' err=' + g.getError(); const x = g.getExtension(ext); return before + ' ext=' + (x ? 'object' : 'null') + ' err=' + g.getError(); });
      }
    }
    await T('gate: enable(DEPTH_CLAMP_EXT) before the extension', () => { const g = document.createElement('canvas').getContext('webgl'); const a = g.isEnabled(34383); const e1 = g.getError(); g.enable(34383); return a + ' ' + e1 + ' ' + g.getError(); });
    await T('extension state enums after enabling', () => { const g = document.createElement('canvas').getContext('webgl2'); g.getExtension('EXT_clip_control'); g.getExtension('WEBGL_polygon_mode'); g.getExtension('EXT_polygon_offset_clamp'); return [g.getParameter(37724), g.getParameter(37725), g.getParameter(2880), g.getParameter(36379)].join(' '); });
    return out.join('\n');
  }
  return 'unknown section ' + section;
}
