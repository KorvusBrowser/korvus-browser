(async () => {
  // The engine's own promise chaining and list reads never reach what a page replaced: Promise.prototype.then,
  // Headers' iterator and its next, DOMParser.parseFromString. The probe itself only awaits (an await on a native
  // promise calls no `then` property), so every recorded call is the engine's.
  const R = [];
  const log = [];
  const take = () => { const l = [...new Set(log)].sort().join(','); log.length = 0; return l; };
  const thenD = Object.getOwnPropertyDescriptor(Promise.prototype, 'then');
  Object.defineProperty(Promise.prototype, 'then', { ...thenD, value: function (...a) { log.push('Promise.then'); return thenD.value.apply(this, a); } });
  const hp = Headers.prototype;
  const itD = Object.getOwnPropertyDescriptor(hp, Symbol.iterator);
  Object.defineProperty(hp, Symbol.iterator, { ...itD, value: function (...a) { log.push('Headers[@@iterator]'); return itD.value.apply(this, a); } });
  const enD = Object.getOwnPropertyDescriptor(hp, 'entries');
  Object.defineProperty(hp, 'entries', { ...enD, value: function (...a) { log.push('Headers.entries'); return enD.value.apply(this, a); } });
  const hiProto = Object.getPrototypeOf(new Headers()[Symbol.iterator]());
  const nxD = Object.getOwnPropertyDescriptor(hiProto, 'next');
  Object.defineProperty(hiProto, 'next', { ...nxD, value: function (...a) { log.push('HeadersIterator.next'); return nxD.value.apply(this, a); } });
  const dpD = Object.getOwnPropertyDescriptor(DOMParser.prototype, 'parseFromString');
  Object.defineProperty(DOMParser.prototype, 'parseFromString', { ...dpD, value: function (...a) { log.push('DOMParser.parseFromString'); return dpD.value.apply(this, a); } });
  take();
  try { const r = await fetch('/nothing', { headers: { 'x-a': 'b' } }); R.push(['fetch status', r.status]); await r.text(); } catch (e) { R.push(['fetch threw', e.name]); }
  R.push(['fetch with headers', take()]);
  await new Promise((res) => { const x = new XMLHttpRequest(); x.onloadend = res; x.open('GET', '/nothing'); x.send(); });
  R.push(['XMLHttpRequest send', take()]);
  await new Promise((res) => { const x = new XMLHttpRequest(); x.onloadend = () => { try { x.responseXML; } catch (e) {} res(); }; x.open('GET', '/frame'); x.responseType = ''; x.overrideMimeType('text/xml'); x.send(); });
  R.push(['XMLHttpRequest responseXML', take()]);
  const w = new Worker(URL.createObjectURL(new Blob(['fetch("/nothing").then(()=>postMessage(1),()=>postMessage(2))'])));
  await new Promise((res) => { w.onmessage = res; setTimeout(res, 2000); });
  w.terminate();
  R.push(['a worker fetch', take()]);
  Object.defineProperty(Promise.prototype, 'then', thenD);
  return R;
})()
