// WebGL context state as Chrome 150 reads it back (an interactive-widget challenge 2026-10-08). Device-independent only:
// every value here is Blink's or GL's rule, never the GPU's, so the recording from one machine is the
// answer on any. Returns [[label, value], ...].
(function () {
  var R = [];
  function val(x) {
    if (x === null || x === undefined) return String(x);
    if (typeof x === 'object') {
      if (x.length !== undefined) return x.constructor.name + '[' + Array.from(x).join(',') + ']';
      return x.constructor.name;
    }
    return x;
  }
  var STATE = [2849, 2884, 2885, 2886, 2928, 2929, 2930, 2931, 2932, 2960, 2961, 2962, 2963, 2964, 2965, 2966, 2967, 2968,
    2978, 3024, 3042, 3088, 3089, 3106, 3107, 3317, 3333, 32773, 32777, 32823, 32824, 32873, 32926, 32928, 32938, 32939,
    32968, 32969, 32970, 32971, 33170, 34016, 34068, 34816, 34817, 34818, 34819, 34877, 34964, 34965, 35725, 36003, 36004,
    36005, 36006, 36007, 37440, 37441, 37443, 34467];
  var STATE2 = [3074, 3314, 3315, 3316, 3330, 3331, 3332, 32874, 32877, 32878, 34229, 35053, 35055, 35097, 35368, 35869,
    35983, 36010, 36389, 36662, 36663, 35723, 36387, 36388, 35977, 34853, 34854, 34855];
  ['webgl', 'webgl2'].forEach(function (v) {
    var cv = document.createElement('canvas');
    var g = cv.getContext(v);
    var all = v === 'webgl2' ? STATE.concat(STATE2) : STATE;
    function snap(tag) {
      all.forEach(function (p) {
        var x; try { x = g.getParameter(p); } catch (e) { x = 'threw ' + e.name; }
        R.push([v + ' ' + tag + ' ' + p, val(x), g.getError()]);
      });
    }
    snap('init');
    // Which names exist at all, before any extension: Chrome accepts 89 / 151 of 0..65535.
    var ok = [];
    for (var p = 0; p < 0x10000; p++) { try { g.getParameter(p); } catch (e) { continue; } if (g.getError() === 0) ok.push(p); }
    R.push([v + ' valid names', ok.join(',')]);
    // Setters, valid and not.
    g.enable(g.BLEND); g.enable(g.CULL_FACE); g.enable(g.DEPTH_TEST); g.enable(g.SCISSOR_TEST); g.enable(g.STENCIL_TEST);
    g.enable(g.POLYGON_OFFSET_FILL); g.enable(g.SAMPLE_ALPHA_TO_COVERAGE); g.enable(g.SAMPLE_COVERAGE); g.disable(g.DITHER);
    g.viewport(1, 2, 30, 40); g.scissor(3, 4, 5, 6); g.clearColor(0.1, 0.2, 0.3, 0.4); g.clearDepth(0.5); g.clearStencil(7);
    g.colorMask(true, false, true, false); g.depthMask(false); g.depthFunc(g.GREATER); g.depthRange(0.25, 0.75);
    g.blendColor(0.5, 0.25, 0.125, 1); g.blendEquationSeparate(g.FUNC_SUBTRACT, g.FUNC_REVERSE_SUBTRACT);
    g.blendFuncSeparate(g.SRC_ALPHA, g.ONE_MINUS_SRC_ALPHA, g.ONE, g.ZERO);
    g.cullFace(g.FRONT); g.frontFace(g.CW); g.lineWidth(1); g.polygonOffset(2, 3); g.sampleCoverage(0.5, true);
    g.stencilFuncSeparate(g.FRONT, g.LESS, 3, 0xF0); g.stencilFuncSeparate(g.BACK, g.GEQUAL, 5, 0x0F);
    g.stencilOpSeparate(g.FRONT, g.INCR, g.DECR, g.INVERT); g.stencilMaskSeparate(g.BACK, 0x33);
    g.hint(g.GENERATE_MIPMAP_HINT, g.NICEST); g.pixelStorei(g.PACK_ALIGNMENT, 2); g.pixelStorei(g.UNPACK_ALIGNMENT, 8);
    g.pixelStorei(g.UNPACK_FLIP_Y_WEBGL, true); g.pixelStorei(g.UNPACK_PREMULTIPLY_ALPHA_WEBGL, true);
    g.pixelStorei(g.UNPACK_COLORSPACE_CONVERSION_WEBGL, 0);
    R.push([v + ' valid setters error', g.getError()]);
    // Each invalid one raises and leaves the state alone.
    var bad = [
      function () { g.depthFunc(0x1234); }, function () { g.enable(0x1234); }, function () { g.cullFace(g.CW); },
      function () { g.frontFace(g.FRONT); }, function () { g.blendFunc(g.CONSTANT_COLOR, g.CONSTANT_ALPHA); },
      function () { g.blendEquation(0x1234); }, function () { g.depthRange(0.8, 0.2); }, function () { g.viewport(0, 0, -1, 5); },
      function () { g.lineWidth(0); }, function () { g.pixelStorei(g.UNPACK_ALIGNMENT, 3); }, function () { g.pixelStorei(0x1234, 1); },
      function () { g.hint(g.GENERATE_MIPMAP_HINT, 0x1234); }, function () { g.stencilFunc(0x1234, 0, 0); },
      function () { g.stencilOp(g.KEEP, 0x1234, g.KEEP); }, function () { g.activeTexture(g.TEXTURE0 + 4096); },
      function () { g.getParameter(0x1234); }, function () { g.isEnabled(0x1234); },
      function () { g.getShaderPrecisionFormat(0x1234, g.HIGH_FLOAT); }, function () { g.getShaderPrecisionFormat(g.VERTEX_SHADER, 0x1234); }
    ];
    bad.forEach(function (f, i) { var r; try { r = f(); } catch (e) { r = 'threw ' + e.name; } R.push([v + ' bad ' + i, val(r), g.getError(), g.getError()]); });
    // Bindings read back the SAME objects.
    g.activeTexture(g.TEXTURE3);
    var t = g.createTexture(); g.bindTexture(g.TEXTURE_2D, t);
    var b = g.createBuffer(); g.bindBuffer(g.ARRAY_BUFFER, b);
    var e = g.createBuffer(); g.bindBuffer(g.ELEMENT_ARRAY_BUFFER, e);
    var f = g.createFramebuffer();
    var rb = g.createRenderbuffer(); g.bindRenderbuffer(g.RENDERBUFFER, rb);
    var vs = g.createShader(g.VERTEX_SHADER); g.shaderSource(vs, 'attribute vec4 p; void main(){gl_Position=p;}'); g.compileShader(vs);
    var fs = g.createShader(g.FRAGMENT_SHADER); g.shaderSource(fs, 'void main(){gl_FragColor=vec4(1.0);}'); g.compileShader(fs);
    var pr = g.createProgram(); g.attachShader(pr, vs); g.attachShader(pr, fs);
    var unlinked = g.createProgram(); g.useProgram(unlinked);
    R.push([v + ' useProgram unlinked', g.getError(), val(g.getParameter(g.CURRENT_PROGRAM))]);
    g.linkProgram(pr); g.useProgram(pr);
    var sh = g.getAttachedShaders(pr);
    R.push([v + ' identity', g.getParameter(g.CURRENT_PROGRAM) === pr, g.getParameter(g.ARRAY_BUFFER_BINDING) === b,
      g.getParameter(g.ELEMENT_ARRAY_BUFFER_BINDING) === e, g.getParameter(g.TEXTURE_BINDING_2D) === t,
      g.getParameter(g.RENDERBUFFER_BINDING) === rb, sh[0] === vs, sh[1] === fs]);
    g.bindBuffer(g.ARRAY_BUFFER, e);
    R.push([v + ' element buffer as array', g.getError(), g.getParameter(g.ARRAY_BUFFER_BINDING) === b]);
    g.bindTexture(g.TEXTURE_CUBE_MAP, t);
    R.push([v + ' 2d texture as cube', g.getError(), val(g.getParameter(g.TEXTURE_BINDING_CUBE_MAP))]);
    snap('set');
    g.bindFramebuffer(g.FRAMEBUFFER, f);
    R.push([v + ' framebuffer', g.getParameter(g.FRAMEBUFFER_BINDING) === f]);
    // Deleting unbinds.
    g.deleteBuffer(b); g.deleteTexture(t); g.deleteFramebuffer(f); g.deleteRenderbuffer(rb); g.deleteProgram(pr);
    R.push([v + ' after delete', val(g.getParameter(g.ARRAY_BUFFER_BINDING)), val(g.getParameter(g.TEXTURE_BINDING_2D)),
      val(g.getParameter(g.FRAMEBUFFER_BINDING)), val(g.getParameter(g.RENDERBUFFER_BINDING)), g.getParameter(g.CURRENT_PROGRAM) === pr, g.getError()]);
    g.bindBuffer(g.ARRAY_BUFFER, b);
    R.push([v + ' bind deleted', g.getError(), val(g.getParameter(g.ARRAY_BUFFER_BINDING))]);
    // Errors: each code once, oldest first, however many times it was raised.
    for (var i = 0; i < 100; i++) g.getParameter(0x1234);
    g.depthRange(1, 0); g.getParameter(0x1235); g.viewport(0, 0, -1, -1);
    var errs = []; for (var j = 0; j < 5; j++) errs.push(g.getError());
    R.push([v + ' error queue', errs.join(',')]);
    // Colour spaces.
    var cs = [g.drawingBufferColorSpace, g.unpackColorSpace];
    g.drawingBufferColorSpace = 'display-p3'; cs.push(g.drawingBufferColorSpace);
    g.drawingBufferColorSpace = 'bogus'; cs.push(g.drawingBufferColorSpace);
    try { g.drawingBufferColorSpace = 'rec2020'; cs.push('no throw'); } catch (x) { cs.push(x.name + ': ' + x.message); }
    g.unpackColorSpace = 'display-p3'; cs.push(g.unpackColorSpace);
    R.push([v + ' color spaces', cs.join(' | ')]);
    // Extensions both a Chrome and its recorded device have, and the names they open.
    var ani = g.getExtension('EXT_texture_filter_anisotropic');
    R.push([v + ' anisotropy type', typeof g.getParameter(34047), g.getError()]);
    if (v === 'webgl') {
      g.getExtension('OES_standard_derivatives');
      R.push([v + ' derivative hint', g.getParameter(35723), g.getError()]);
      g.hint(35723, g.NICEST); R.push([v + ' derivative hint set', g.getParameter(35723), g.getError()]);
      R.push([v + ' vao before', val(g.getParameter(34229)), g.getError()]);
      var vx = g.getExtension('OES_vertex_array_object'); var va = vx.createVertexArrayOES(); vx.bindVertexArrayOES(va);
      R.push([v + ' vao', g.getParameter(34229) === va, g.getError()]);
      R.push([v + ' timestamp before', val(g.getParameter(36392)), g.getError()]);
    }
    var s3 = g.getExtension('WEBGL_compressed_texture_s3tc');
    if (s3) R.push([v + ' compressed formats', val(g.getParameter(g.COMPRESSED_TEXTURE_FORMATS))]);
    if (v === 'webgl2') {
      var IF = [32856, 33321, 36208, 6408, 6407, 36194, 33189, 35056, 34842, 34836, 33326, 0x1234];
      IF.forEach(function (fmt) {
        var x = g.getInternalformatParameter(g.RENDERBUFFER, fmt, g.SAMPLES);
        R.push([v + ' internalformat ' + fmt, x === null ? 'null' : x.constructor.name + (x.length ? ' nonempty' : ' empty'), g.getError()]);
      });
      R.push([v + ' internalformat bad target', val(g.getInternalformatParameter(g.TEXTURE_2D, 32856, g.SAMPLES)), g.getError()]);
      R.push([v + ' internalformat bad pname', val(g.getInternalformatParameter(g.RENDERBUFFER, 32856, 0x1234)), g.getError()]);
      R.push([v + ' internalformat int bad pname', val(g.getInternalformatParameter(g.RENDERBUFFER, 36208, 0x1234)), g.getError()]);
      g.getExtension('EXT_color_buffer_float');
      var y = g.getInternalformatParameter(g.RENDERBUFFER, 34836, g.SAMPLES);
      R.push([v + ' internalformat float', y === null ? 'null' : y.constructor.name, g.getError()]);
      var smp = g.createSampler(); g.bindSampler(3, smp);
      var ub = g.createBuffer(); g.bindBufferBase(g.UNIFORM_BUFFER, 0, ub);
      var t3 = g.createTexture(); g.bindTexture(g.TEXTURE_3D, t3);
      g.readBuffer(g.NONE); g.drawBuffers([g.NONE]);
      R.push([v + ' v2 bindings', g.getParameter(g.SAMPLER_BINDING) === smp, g.getParameter(g.UNIFORM_BUFFER_BINDING) === ub,
        g.getParameter(g.TEXTURE_BINDING_3D) === t3, g.getParameter(g.READ_BUFFER), g.getParameter(34853), g.getError()]);
      g.readBuffer(g.COLOR_ATTACHMENT0); R.push([v + ' readBuffer default fb attachment', g.getError()]);
      g.enable(g.RASTERIZER_DISCARD); R.push([v + ' rasterizer discard', g.getParameter(g.RASTERIZER_DISCARD), g.isEnabled(g.RASTERIZER_DISCARD), g.getError()]);
    }
  });
  return R;
})()
