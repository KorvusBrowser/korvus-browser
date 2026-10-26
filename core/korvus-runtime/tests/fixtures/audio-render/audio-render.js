// Web Audio rendered as Chrome 150 renders it on x86 Linux (an interactive-widget challenge 2026-10-08): every sample of
// OfflineAudioContext renders -- the graph a managed bot challenge builds (a triangle at
// 9998.123456 Hz through a compressor), each built-in waveform, constructor options, the compressor
// on DC and on a sine, a low frequency (Lagrange interpolation). Resolves to [[label, [samples]], ...].
// The first quantum after `frequency.value = f` takes the sample-accurate path, the rest the k-rate one.
(function () {
  function ctx() { return new OfflineAudioContext(1, 1200, 44100); }
  function osc(c, type, f) { var o = c.createOscillator(); o.type = type; o.frequency.value = f; return o; }
  function comp(c) {
    var d = c.createDynamicsCompressor();
    d.threshold.value = -52; d.knee.value = 40; d.ratio.value = 12; d.attack.value = 0.0001; d.release.value = 0.25;
    return d;
  }
  var cases = [];
  // a challenge provider's: the oscillator started, through the compressor.
  (function () { var c = ctx(), o = osc(c, 'triangle', 9998.123456), d = comp(c); o.connect(d); d.connect(c.destination); o.start(0); cases.push(['challenge graph', c]); })();
  ['sine', 'square', 'sawtooth', 'triangle'].forEach(function (t) {
    var c = ctx(), o = osc(c, t, t === 'sine' ? 440 : 1000); o.connect(c.destination); o.start(0); cases.push([t, c]);
  });
  // Constructor options set the value WITHOUT an automation event (k-rate from the first quantum).
  (function () { var c = ctx(), o = new OscillatorNode(c, { type: 'triangle', frequency: 777.7 }); o.connect(c.destination); o.start(0); cases.push(['options', c]); })();
  (function () { var c = ctx(), s = c.createConstantSource(), d = comp(c); s.offset.value = 0.9; s.connect(d); d.connect(c.destination); s.start(0); cases.push(['compressor dc', c]); })();
  (function () { var c = ctx(), o = osc(c, 'sine', 440), d = comp(c); o.connect(d); d.connect(c.destination); o.start(0); cases.push(['compressor sine', c]); })();
  (function () { var c = ctx(), o = osc(c, 'sine', 2); o.connect(c.destination); o.start(0); cases.push(['low frequency', c]); })();
  return Promise.all(cases.map(function (x) { return x[1].startRendering(); })).then(function (bufs) {
    return bufs.map(function (b, i) { return [cases[i][0], Array.from(b.getChannelData(0))]; });
  });
})()
