(async function () {
  /* OfflineAudioContext.startRendering(): the states the page sees, the statechange events, the order of the
     complete listener / oncomplete / the promise, whether the page's own tasks run while it renders (Chrome renders
     on its offline render thread), and the classic fingerprint's samples. No timings: those are the machine's. */
  var out = [];
  var run = async function (label, frames, build) {
    var ctx = new OfflineAudioContext(1, frames, 44100);
    build(ctx);
    var order = [], changes = [];
    var ticks = 0, done = false;
    var mc = new MessageChannel();
    mc.port1.onmessage = function () { if (!done) { ticks++; mc.port2.postMessage(0); } };
    ctx.addEventListener('complete', function (e) { order.push('listener:' + ctx.state + ':' + (e.renderedBuffer && e.renderedBuffer.length)); });
    ctx.oncomplete = function () { order.push('oncomplete'); };
    ctx.onstatechange = function () { changes.push(ctx.state); };
    var s0 = ctx.state;
    var settled = false;
    var p = ctx.startRendering();
    p.then(function () { settled = true; });
    var s1 = ctx.state;
    mc.port2.postMessage(0);
    await Promise.resolve();
    var settledAfterMicrotask = settled;
    var buf = await p.then(function (b) { order.push('promise'); return b; });
    done = true;
    await new Promise(function (r) { setTimeout(r, 30); });
    var d = buf.getChannelData(0);
    var sum = 0; for (var i = 4500; i < Math.min(5000, d.length); i++) sum += Math.abs(d[i]);
    out.push([label, s0 + '>' + s1 + '>' + ctx.state, 'statechange ' + changes.join('|'), order.join(','), 'tasks ran while rendering ' + (ticks > 0),
      'settled in the call\'s microtask ' + settledAfterMicrotask, buf.length, buf.sampleRate, sum]);
  };
  var fp = function (ctx) {
    var o = ctx.createOscillator(); o.type = 'triangle'; o.frequency.value = 10000;
    var c = ctx.createDynamicsCompressor();
    c.threshold.value = -50; c.knee.value = 40; c.ratio.value = 12; c.attack.value = 0; c.release.value = 0.25;
    o.connect(c); c.connect(ctx.destination); o.start(0);
  };
  await run('fingerprint 44100', 44100, fp);
  await run('fingerprint 441000', 441000, fp);
  var again = new OfflineAudioContext(1, 128, 44100);
  await again.startRendering();
  out.push(['startRendering twice', await again.startRendering().then(function () { return 'resolved'; }, function (e) { return e.name + ': ' + e.message; })]);
  return out;
})()
