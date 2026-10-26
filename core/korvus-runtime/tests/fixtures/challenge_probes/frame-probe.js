function race(p, ms) { return Promise.race([p, new Promise(r => setTimeout(() => r('TIMEOUT'), ms))]); }
function d(o, k) { const x = Object.getOwnPropertyDescriptor(o, k); return x ? Object.keys(x).filter(n => x[n] !== undefined && typeof x[n] !== 'function' || n === 'get' || n === 'set').join(',') + (x.get ? ':get' : '') : 'none'; }
async function run() {
  const r = {};
  const S = (k, f) => { try { r[k] = f(); } catch (e) { r[k] = 'THROW ' + e.name + ': ' + e.message; } };
  S('notif', () => typeof Notification === 'undefined' ? 'no Notification' : Notification.permission);
  try { r.notifQuery = (await navigator.permissions.query({ name: 'notifications' })).state; } catch (e) { r.notifQuery = 'THROW ' + e.name; }
  try {
    const b = await race(navigator.getBattery(), 2000);
    r.battery = b === 'TIMEOUT' ? b : [Object.prototype.toString.call(b), b.charging, b.level, b.chargingTime, String(b.dischargingTime)];
  } catch (e) { r.battery = 'THROW ' + e.name + ': ' + e.message; }
  S('memory', () => { const m = performance.memory; return [typeof m, Object.prototype.toString.call(m), Object.getOwnPropertyNames(m).join(','), Object.getOwnPropertyNames(Object.getPrototypeOf(m)).join(','), d(Object.getPrototypeOf(m), 'jsHeapSizeLimit'), m.jsHeapSizeLimit, typeof m.totalJSHeapSize, typeof m.usedJSHeapSize, m.totalJSHeapSize % 100000 === 0]; });
  S('memoryDesc', () => d(Performance.prototype, 'memory'));
  S('modelContext', () => ['modelContext' in navigator, typeof navigator.modelContext, Object.prototype.toString.call(navigator.modelContext), d(Navigator.prototype, 'modelContext')]);
  S('rtcAudio', () => { const c = RTCRtpReceiver.getCapabilities('audio'); return c && [c.codecs.length, c.codecs.slice(0, 3).map(x => x.mimeType + '/' + x.clockRate + '/' + (x.channels || '') + '/' + (x.sdpFmtpLine || '')), c.headerExtensions.length, c.headerExtensions.slice(0, 2).map(x => x.uri)]; });
  S('rtcVideo', () => { const c = RTCRtpReceiver.getCapabilities('video'); return c && [c.codecs.length, c.codecs.slice(0, 4).map(x => x.mimeType + '/' + x.clockRate + '/' + (x.sdpFmtpLine || '')), c.headerExtensions.length]; });
  S('rtcSendAudio', () => { const c = RTCRtpSender.getCapabilities('audio'); return c && [c.codecs.length, c.headerExtensions.length]; });
  S('rtcBad', () => RTCRtpReceiver.getCapabilities('foo'));
  S('hevc', () => ['video/mp4; codecs="hev1.1.6.L93.B0"', 'video/mp4; codecs="hvc1.1.6.L93.B0"', 'video/mp4; codecs="avc1.42E01E"', 'video/webm; codecs="vp9"', 'video/mp4; codecs="av01.0.05M.08"'].map(t => MediaSource.isTypeSupported(t)));
  S('hevcCanPlay', () => { const v = document.createElement('video'); return ['video/mp4; codecs="hev1.1.6.L93.B0"', 'video/mp4; codecs="hvc1.1.6.L93.B0"'].map(t => v.canPlayType(t)); });
  try {
    const u = URL.createObjectURL(new Blob(['postMessage({a: 1, b: "x", c: [1, 2], d: {e: 1}})'], { type: 'text/javascript' }));
    const w = new Worker(u);
    r.workerData = await race(new Promise(res => { w.onmessage = e => res([e.data.constructor === Object, Object.getPrototypeOf(e.data) === Object.prototype, Object.prototype.toString.call(e.data), e.data.constructor && e.data.constructor.name, Object.getOwnPropertyNames(e.data).join(','), Array.isArray(e.data.c), Object.getPrototypeOf(e.data.d) === Object.prototype, e.isTrusted]); }), 3000);
    w.terminate();
  } catch (e) { r.workerData = 'THROW ' + e; }
  S('computed', () => { const cs = getComputedStyle(document.getElementById('d')); return [cs.length, cs[0], cs[1], cs[cs.length - 1], cs.accentColor, Object.getOwnPropertyNames(cs).length, d(cs, '0'), d(cs, 'accentColor'), d(CSSStyleDeclaration.prototype, 'accentColor'), 'accentColor' in cs, Object.keys(cs).length, cs.getPropertyValue('display')]; });
  S('styleEnum', () => { const cs = getComputedStyle(document.getElementById('d')); const names = []; for (let i = 0; i < cs.length; i++) names.push(cs[i]); return [names.length, names.slice(0, 5).join(','), names.slice(-5).join(',')]; });
  try {
    r.img = await race(new Promise(res => { const img = new Image(); img.onload = () => { let px; try { const c = document.createElement('canvas'); c.width = img.naturalWidth; c.height = img.naturalHeight; const x = c.getContext('2d'); x.drawImage(img, 0, 0); const q = x.getImageData(0, 0, c.width, c.height).data; let h = 0; for (let i = 0; i < q.length; i++) h = (h * 31 + q[i]) >>> 0; px = [q.length, h]; } catch (e) { px = String(e); } res(['load', img.width, img.height, img.naturalWidth, img.naturalHeight, img.complete, px]); }; img.onerror = () => res(['error', img.width, img.naturalWidth, img.complete]); img.src = '/ci1.png'; }), 3000);
  } catch (e) { r.img = 'THROW ' + e; }
  try { const b = await (await fetch('/ci1.png')).blob(); const bm = await createImageBitmap(b); r.bitmap = [bm.width, bm.height]; } catch (e) { r.bitmap = 'THROW ' + e.name + ': ' + e.message; }
  try { const im = new Image(); im.src = '/ci1.png'; await im.decode(); r.decode = [im.naturalWidth, im.naturalHeight]; } catch (e) { r.decode = 'THROW ' + e.name + ': ' + e.message; }
  await new Promise(res => setTimeout(res, 600));
  S('paint', () => performance.getEntriesByType('paint').map(e => e.name).join(','));
  S('entryTypes', () => [...new Set(performance.getEntries().map(e => e.entryType))].join(','));
  S('screen', () => [screenX, screenY, outerWidth, outerHeight, innerWidth, innerHeight]);
  return r;
}
