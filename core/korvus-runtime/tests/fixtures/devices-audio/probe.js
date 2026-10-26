(async function () {
  /* decodeAudioData (error type, which task answers, the ArrayBuffer transferred, the rate conversion) and
     enumerateDevices (MediaDeviceInfo objects, the task that answers). Rows are deterministic: only orders Chrome
     keeps across runs are recorded -- the answer against the call's own microtask, the promise against the
     callback. Where it lands among the page's timers depends on Chrome's scheduler (a 0 ms timer queued right after
     the call ran before it in some runs, after it in others) and is left out. */
  var out = [];
  var sleep = function (ms) { return new Promise(function (r) { setTimeout(r, ms); }); };
  var E = function (e) { return (e && e.constructor && e.constructor.name) + ':' + (e && e.name) + ':' + (e && e.message) + ':' + (e instanceof DOMException) + ':' + (e && e.code); };
  var wav = function (rate, bits, n, chans) {
    var bps = bits / 8, b = new ArrayBuffer(44 + n * bps * chans), d = new DataView(b);
    var s = function (o, t) { for (var i = 0; i < t.length; i++) d.setUint8(o + i, t.charCodeAt(i)); };
    s(0, 'RIFF'); d.setUint32(4, 36 + n * bps * chans, true); s(8, 'WAVE'); s(12, 'fmt '); d.setUint32(16, 16, true);
    d.setUint16(20, 1, true); d.setUint16(22, chans, true); d.setUint32(24, rate, true); d.setUint32(28, rate * bps * chans, true);
    d.setUint16(32, bps * chans, true); d.setUint16(34, bits, true); s(36, 'data'); d.setUint32(40, n * bps * chans, true);
    for (var i = 0; i < n * chans; i++) {
      var v = Math.round(Math.sin(i * 0.37) * 0.8 * (bits === 16 ? 32767 : 127));
      if (bits === 16) d.setInt16(44 + i * 2, v, true); else d.setUint8(44 + i, v + 128);
    }
    return b;
  };
  var digest = function (arr) { var h = 0; for (var i = 0; i < arr.length; i++) { h = (h * 31 + Math.round(arr[i] * 1e7)) | 0; } return h; };
  var ctx = new AudioContext();
  out.push(['context rate', ctx.sampleRate]);
  var run = async function (label, buf, withCbs) {
    var log = [];
    var p;
    try {
      p = withCbs
        ? ctx.decodeAudioData(buf, function (x) { log.push('successCb ' + Object.prototype.toString.call(x) + ' this=' + (this === ctx)); }, function (e) { log.push('errorCb ' + E(e) + ' this=' + (this === ctx)); })
        : ctx.decodeAudioData(buf);
    } catch (e) { out.push([label, 'THREW ' + E(e)]); return null; }
    log.push('returned byteLength=' + (buf && buf.byteLength));
    Promise.resolve().then(function () { log.push('microtask'); });
    var got = null;
    p.then(function (b) { got = b; log.push('resolved ' + Object.prototype.toString.call(b)); },
           function (e) { log.push('rejected ' + E(e)); });
    await sleep(400);
    out.push([label, log]);
    return got;
  };
  await run('8 zero bytes, callbacks', new ArrayBuffer(8), true);
  await run('8 zero bytes, promise only', new ArrayBuffer(8), false);
  await run('empty buffer, callbacks', new ArrayBuffer(0), true);
  var w = wav(8000, 16, 300, 1);
  var b1 = await run('16-bit 8 kHz mono, callbacks', w, true);
  if (b1) out.push(['16-bit 8 kHz decoded', b1.numberOfChannels, b1.length, b1.sampleRate, digest(b1.getChannelData(0)), Array.prototype.slice.call(b1.getChannelData(0), 0, 6).map(function (x) { return x.toFixed(7); }).join(',')]);
  await run('detached (same buffer again), callbacks', w, true);
  var b2 = await run('8-bit 48 kHz stereo, promise only', wav(48000, 8, 200, 2), false);
  if (b2) out.push(['8-bit 48 kHz decoded', b2.numberOfChannels, b2.length, b2.sampleRate, digest(b2.getChannelData(0)), digest(b2.getChannelData(1))]);
  var b3 = await run('16-bit 22050 Hz mono, promise only', wav(22050, 16, 1000, 1), false);
  if (b3) out.push(['16-bit 22050 Hz decoded', b3.numberOfChannels, b3.length, b3.sampleRate, digest(b3.getChannelData(0))]);
  await run('Uint8Array, callbacks', new Uint8Array(8), true);
  try { ctx.close(); } catch (e) {}

  var order = [];
  var pe = navigator.mediaDevices.enumerateDevices();
  Promise.resolve().then(function () { order.push('microtask'); });
  var ds = await pe.then(function (x) { order.push('resolved'); return x; });
  await sleep(50);
  out.push(['enumerateDevices order', order.join(',')]);
  out.push(['enumerateDevices', ds.length, Object.prototype.toString.call(ds), Array.isArray(ds)]);
  ds.forEach(function (d, i) {
    var r = [Object.prototype.toString.call(d), d instanceof MediaDeviceInfo, typeof InputDeviceInfo === 'function' && d instanceof InputDeviceInfo,
      Object.getOwnPropertyNames(d).join(','), (function () { var k = []; for (var x in d) k.push(x); return k.join(','); })(),
      d.deviceId, d.kind, d.label, d.groupId, JSON.stringify(d), JSON.stringify(d.toJSON()), Object.getPrototypeOf(d.toJSON()) === Object.prototype];
    out.push(['device ' + i, r]);
  });
  var ds2 = await navigator.mediaDevices.enumerateDevices();
  out.push(['same objects across calls', ds.length && ds2.length ? ds[0] === ds2[0] : 'n/a']);
  out.push(['MediaDeviceInfo ctor', (function () { try { new MediaDeviceInfo(); return 'constructed'; } catch (e) { return e.name + ': ' + e.message; } })()]);
  out.push(['getter on proto', (function () { try { return MediaDeviceInfo.prototype.kind; } catch (e) { return e.name + ': ' + e.message; } })()]);
  out.push(['toJSON on plain', (function () { try { return MediaDeviceInfo.prototype.toJSON.call({}); } catch (e) { return e.name + ': ' + e.message; } })()]);
  out.push(['proto keys', Object.getOwnPropertyNames(MediaDeviceInfo.prototype).join(','), typeof InputDeviceInfo === 'function' ? Object.getOwnPropertyNames(InputDeviceInfo.prototype).join(',') : 'none']);
  return out;
})()
