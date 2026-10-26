(async function () {
  /* Every object an API hands the page that the ENGINE made -- a toJSON() result, a dictionary, a sequence, an
     iterator result -- reachable without a permission or a device: what it is (its [[Prototype]] named against THIS
     realm's intrinsics), its own keys in order, the first key's [writable, enumerable, configurable], and each value
     (a number as String() with -0 kept, a string as JSON, an object by its prototype). `T` rows print only typeof per
     key, for values that are not deterministic (clocks, quotas). Left out: WebGL's context attributes (the recording
     Chrome under Xvfb has no WebGL) and whether a DOMQuad's points are its own objects (they are in Chrome; korvus
     mints a new DOMPoint per read). */
  var out = [];
  var OP = Object.prototype, AP = Array.prototype;
  function protoName(o, realm) {
    var p = Object.getPrototypeOf(o);
    if (p === null) return 'NULL';
    if (p === OP) return 'Object.prototype';
    if (p === AP) return 'Array.prototype';
    if (realm && p === realm.Object.prototype) return 'frame Object.prototype';
    if (realm && p === realm.Array.prototype) return 'frame Array.prototype';
    return 'other ' + Object.prototype.toString.call(o);
  }
  function fmt(v) {
    if (v === null) return 'null';
    if (typeof v === 'number') return Object.is(v, -0) ? '-0' : String(v);
    if (typeof v === 'string') return JSON.stringify(v);
    if (typeof v === 'object') return '[' + protoName(v) + (Array.isArray(v) ? ' len ' + v.length : '') + ']';
    return typeof v === 'undefined' ? 'undefined' : typeof v + ':' + String(v);
  }
  function desc(o, k) { var d = Object.getOwnPropertyDescriptor(o, k); return d ? (d.writable ? 'w' : '-') + (d.enumerable ? 'e' : '-') + (d.configurable ? 'c' : '-') + ('get' in d ? 'A' : '') : 'none'; }
  function shape(o, values, realm) {
    if (o === null || typeof o !== 'object') return ['not-object', fmt(o)];
    var ks = Object.keys(o);
    var r = [protoName(o, realm), Array.isArray(o) ? 'array' : 'object', ks.join(','), ks.length ? desc(o, ks[0]) : '-'];
    if (Array.isArray(o)) r.push(Object.isFrozen(o) ? 'frozen' : 'unfrozen');
    r.push(ks.map(function (k) { return values ? fmt(o[k]) : (o[k] === null ? 'null' : typeof o[k]); }).join(' '));
    return r;
  }
  async function row(label, f, values, realm) {
    try {
      var v = await Promise.race([Promise.resolve().then(f), new Promise(function (r) { setTimeout(function () { r(TIMEOUT); }, 3000); })]);
      if (v === TIMEOUT) { out.push([label, 'TIMEOUT']); return; }
      out.push([label].concat(shape(v, values !== false, realm)));
    }
    catch (e) { out.push([label, 'ERR', String(e && e.name), String(e && e.message).slice(0, 100)]); }
  }
  var V = true, T = false, TIMEOUT = {};

  // ── toJSON() of the geometry interfaces (op2 #[serde] returns in korvus) ─────────────────────────────────────
  await row('DOMRect.toJSON', function () { return new DOMRect(1, 2, 3, 4).toJSON(); }, V);
  await row('DOMRectReadOnly.toJSON', function () { return new DOMRectReadOnly(1, 2, 3, 4).toJSON(); }, V);
  await row('DOMRect.toJSON non-finite', function () { return new DOMRect(NaN, -0, Infinity, -Infinity).toJSON(); }, V);
  await row('DOMRect.toJSON NaN width', function () { return new DOMRect(1, 2, NaN, 4).toJSON(); }, V);
  await row('DOMRect.toJSON negative', function () { return new DOMRect(10, 20, -3, -4).toJSON(); }, V);
  await row('DOMRect.toJSON signed zeros', function () { return new DOMRect(-0, -0, 0, 0).toJSON(); }, V);
  await row('DOMRect edges NaN height', function () { var r = new DOMRect(1, NaN, 2, 3); return [r.top, r.bottom, r.left, r.right]; }, V);
  await row('DOMRect.toJSON foreign receiver', function () { return DOMRect.prototype.toJSON.call({}); }, V);
  await row('DOMQuad.toJSON foreign receiver', function () { return DOMQuad.prototype.toJSON.call({}); }, V);
  await row('DOMQuad.getBounds NaN', function () { return new DOMQuad({ x: NaN }, { x: 4, y: 2 }).getBounds().toJSON(); }, V);
  await row('DOMQuad.getBounds signed zeros', function () { return new DOMQuad({ x: -0, y: -0 }, { x: 0, y: 0 }, { x: -0, y: 0 }, { x: 0, y: -0 }).getBounds().toJSON(); }, V);
  await row('DOMRect.fromRect.toJSON', function () { return DOMRect.fromRect({ x: 5, height: 7 }).toJSON(); }, V);
  await row('DOMPoint.toJSON', function () { return new DOMPoint(1, 2, 3, 4).toJSON(); }, V);
  await row('DOMPointReadOnly.toJSON', function () { return new DOMPointReadOnly(1, 2).toJSON(); }, V);
  await row('DOMPoint.toJSON non-finite', function () { return new DOMPoint(NaN, -0, Infinity).toJSON(); }, V);
  await row('DOMMatrix.toJSON', function () { return new DOMMatrix([1, 2, 3, 4, 5, 6]).toJSON(); }, V);
  await row('DOMMatrixReadOnly.toJSON', function () { return new DOMMatrixReadOnly().toJSON(); }, V);
  await row('DOMMatrix3d.toJSON', function () { return new DOMMatrix([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 7, 8, 9, 1]).toJSON(); }, V);
  await row('DOMQuad.toJSON', function () { return new DOMQuad({ x: 1 }, { y: 2 }).toJSON(); }, V);
  await row('DOMQuad.toJSON.p1', function () { return new DOMQuad({ x: 1 }).toJSON().p1; }, V);
  await row('DOMQuad.getBounds.toJSON', function () { return new DOMQuad({ x: 1 }, { x: 4, y: 2 }).getBounds().toJSON(); }, V);
  await row('Element.getBoundingClientRect.toJSON', function () { return document.body.getBoundingClientRect().toJSON(); }, T);
  await row('Element.getClientRects[0].toJSON', function () {
    var d = document.createElement('span'); d.textContent = 'x'; document.body.appendChild(d);
    var r = d.getClientRects()[0].toJSON(); d.remove(); return r;
  }, T);
  await row('Range.getBoundingClientRect.toJSON', function () { var r = document.createRange(); r.selectNodeContents(document.body); return r.getBoundingClientRect().toJSON(); }, T);
  await row('DOMRect.toJSON hasOwnProperty walk', function () {
    var m = new DOMRect(1, 2, 3, 4).toJSON(), s = '';
    for (var C in m) m.hasOwnProperty(C) && (s += m[C]);
    return [s];
  }, V);
  await row('JSON.stringify(DOMRect)', function () { return [JSON.stringify(new DOMRect(1, 2, 3, 4))]; }, V);
  await row('DOMRect.toJSON is a new object', function () { var r = new DOMRect(); return [r.toJSON() !== r.toJSON(), Object.isExtensible(r.toJSON())]; }, V);
  await row('toJSON defines, never sets', function () {
    var log = [], ks = ['x', 'left', 'p1', 'is2D', 'read'];
    ks.forEach(function (k) { Object.defineProperty(OP, k, { configurable: true, set: function (v) { log.push(k); } }); });
    try {
      var a = new DOMRect(1, 2, 3, 4).toJSON(), q = new DOMQuad({ x: 1 }).toJSON(), m = new DOMMatrix().toJSON(), t = new TextEncoder().encodeInto('a', new Uint8Array(2));
      return [log.join(','), Object.keys(a).join(','), Object.keys(q).join(','), Object.keys(m).length, Object.keys(t).join(','), a.x, a.left];
    } finally { ks.forEach(function (k) { delete OP[k]; }); }
  }, V);
  await row('DOMQuad.toJSON reads its own points', function () {
    var q = new DOMQuad({ x: 1 }), d = Object.getOwnPropertyDescriptor(DOMQuad.prototype, 'p1');
    Object.defineProperty(DOMQuad.prototype, 'p1', { configurable: true, get: function () { return 'page'; } });
    try { var j = q.toJSON(); return [typeof j.p1, String(j.p1 && j.p1.x)]; } finally { Object.defineProperty(DOMQuad.prototype, 'p1', d); }
  }, V);

  // ── the other #[serde] returns ─────────────────────────────────────────────────────────────────────────────
  await row('TextEncoder.encodeInto', function () { return new TextEncoder().encodeInto(String.fromCharCode(0x61, 0x62, 0xe9, 0xd83d, 0xde00), new Uint8Array(6)); }, V);
  await row('TextEncoder.encodeInto empty', function () { return new TextEncoder().encodeInto('', new Uint8Array(0)); }, V);
  await row('Headers.getSetCookie', function () { var h = new Headers([['Set-Cookie', 'a=1'], ['set-cookie', 'b=2'], ['x', 'y']]); return h.getSetCookie(); }, V);
  await row('Headers.getSetCookie empty', function () { return new Headers().getSetCookie(); }, V);
  await row('FormData.getAll', function () { var f = new FormData(); f.append('a', '1'); f.append('a', '2'); return f.getAll('a'); }, V);
  await row('FormData.getAll none', function () { return new FormData().getAll('a'); }, V);
  await row('URLSearchParams.getAll', function () { return new URLSearchParams('a=1&a=2').getAll('a'); }, V);

  // ── dictionaries and sequences other bindings return ───────────────────────────────────────────────────────
  var c2 = document.createElement('canvas').getContext('2d');
  await row('2d.getContextAttributes', function () { return c2.getContextAttributes(); }, V);
  await row('OffscreenCanvas2d.getContextAttributes', function () { return new OffscreenCanvas(1, 1).getContext('2d').getContextAttributes(); }, V);
  await row('2d.measureText', function () { return Object.getPrototypeOf(c2.measureText('x')) === TextMetrics.prototype ? ['TextMetrics'] : ['other']; }, V);
  await row('userAgentData.toJSON', function () { return navigator.userAgentData.toJSON(); }, T);
  await row('userAgentData.toJSON.brands[0]', function () { return navigator.userAgentData.toJSON().brands[0]; }, T);
  await row('userAgentData.brands[0]', function () { return navigator.userAgentData.brands[0]; }, T);
  await row('getHighEntropyValues', function () { return navigator.userAgentData.getHighEntropyValues(['architecture', 'bitness', 'model', 'platformVersion', 'uaFullVersion', 'wow64']); }, T);
  await row('getHighEntropyValues.fullVersionList[0]', async function () { return (await navigator.userAgentData.getHighEntropyValues(['fullVersionList'])).fullVersionList[0]; }, T);
  await row('storage.estimate', function () { return navigator.storage.estimate(); }, T);
  await row('locks.query', function () { return navigator.locks.query(); }, V);
  await row('mediaDevices.getSupportedConstraints', function () { return navigator.mediaDevices.getSupportedConstraints(); }, V);
  await row('mediaCapabilities.decodingInfo', function () { return navigator.mediaCapabilities.decodingInfo({ type: 'file', video: { contentType: 'video/webm; codecs="vp8"', width: 640, height: 480, bitrate: 10000, framerate: 30 } }); }, T);
  await row('AudioContext.getOutputTimestamp', function () { var a = new AudioContext(); var t = a.getOutputTimestamp(); a.close(); return t; }, T);
  await row('crypto.subtle.exportKey(jwk)', async function () { var k = await crypto.subtle.importKey('raw', new Uint8Array(16), { name: 'HMAC', hash: 'SHA-256' }, true, ['sign']); return crypto.subtle.exportKey('jwk', k); }, V);
  await row('crypto.subtle.generateKey(ECDSA)', function () { return crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign']); }, T);
  await row('CryptoKey.algorithm', async function () { var k = await crypto.subtle.importKey('raw', new Uint8Array(16), { name: 'HMAC', hash: 'SHA-256' }, true, ['sign']); return k.algorithm; }, T);
  await row('CryptoKey.algorithm.hash', async function () { var k = await crypto.subtle.importKey('raw', new Uint8Array(16), { name: 'HMAC', hash: 'SHA-256' }, true, ['sign']); return k.algorithm.hash; }, V);
  await row('CryptoKey.usages', async function () { var k = await crypto.subtle.importKey('raw', new Uint8Array(16), { name: 'HMAC', hash: 'SHA-256' }, true, ['verify', 'sign']); return k.usages; }, V);

  // ── iterator results and stream reads ─────────────────────────────────────────────────────────────────────
  await row('Headers.entries.next', function () { return new Headers({ a: '1' }).entries().next(); }, V);
  await row('Headers.entries.next.value', function () { return new Headers({ a: '1' }).entries().next().value; }, V);
  await row('Headers.entries done', function () { var i = new Headers().entries(); return i.next(); }, V);
  await row('FormData.entries.next', function () { var f = new FormData(); f.append('a', 'b'); return f.entries().next(); }, V);
  await row('URLSearchParams.entries.next', function () { return new URLSearchParams('a=b').entries().next(); }, V);
  await row('NodeList.entries.next', function () { return document.body.childNodes.entries().next(); }, T);
  await row('DOMTokenList.values.next', function () { var d = document.createElement('div'); d.className = 'q'; return d.classList.values().next(); }, V);
  await row('ReadableStream.read', function () { var r = new ReadableStream({ start: function (c) { c.enqueue(1); } }).getReader(); return r.read(); }, V);
  await row('ReadableStream.read done', function () { var r = new ReadableStream({ start: function (c) { c.close(); } }).getReader(); return r.read(); }, V);
  await row('ReadableStream async iterator next', function () { var s = new ReadableStream({ start: function (c) { c.enqueue('z'); } }); return s[Symbol.asyncIterator]().next(); }, V);

  // ── performance entries ───────────────────────────────────────────────────────────────────────────────────
  await row('PerformanceMark.toJSON', function () { return performance.mark('m', { detail: 1 }).toJSON(); }, T);
  await row('PerformanceMeasure.toJSON', function () { return performance.measure('mm', 'm').toJSON(); }, T);
  await row('PerformanceNavigationTiming.toJSON', function () { return performance.getEntriesByType('navigation')[0].toJSON(); }, T);
  await row('performance.timing.toJSON', function () { return performance.timing.toJSON(); }, T);
  await row('performance.navigation.toJSON', function () { return performance.navigation.toJSON(); }, V);
  await row('performance.toJSON', function () { return performance.toJSON(); }, T);
  await row('performance.getEntries', function () { return performance.getEntriesByName('m'); }, T);
  await row('PerformanceObserver.supportedEntryTypes', function () { return PerformanceObserver.supportedEntryTypes; }, V);

  // ── misc sequences ────────────────────────────────────────────────────────────────────────────────────────
  await row('Element.getAttributeNames', function () { var d = document.createElement('div'); d.setAttribute('b', ''); d.setAttribute('a', ''); return d.getAttributeNames(); }, V);
  await row('MutationObserver.takeRecords', function () { return new MutationObserver(function () {}).takeRecords(); }, V);
  await row('navigator.languages', function () { return navigator.languages; }, T);
  await row('document.getAnimations', function () { return document.getAnimations(); }, V);

  // ── realms: a toJSON() object is the CURRENT realm's (the function's), a dictionary or a sequence the RECEIVER's ──
  var fr = document.createElement('iframe');
  document.body.appendChild(fr);
  var W = fr.contentWindow;
  await row('frame DOMRect.toJSON on a main rect', function () { return W.DOMRect.prototype.toJSON.call(new DOMRect(1, 2, 3, 4)); }, V, W);
  await row('main DOMRect.toJSON on a frame rect', function () { return DOMRect.prototype.toJSON.call(new W.DOMRect(1, 2, 3, 4)); }, V, W);
  await row('frame DOMPoint.toJSON', function () { return new W.DOMPoint(1).toJSON(); }, V, W);
  await row('frame DOMMatrix.toJSON keys', function () { return new W.DOMMatrix().toJSON(); }, T, W);
  await row('frame TextEncoder.encodeInto on a main encoder', function () { return W.TextEncoder.prototype.encodeInto.call(new TextEncoder(), 'a', new Uint8Array(4)); }, V, W);
  await row('frame Headers.getSetCookie on main headers', function () { return W.Headers.prototype.getSetCookie.call(new Headers([['set-cookie', 'a']])); }, V, W);
  await row('frame FormData.getAll on a main form data', function () { var f = new FormData(); f.append('k', 'v'); return W.FormData.prototype.getAll.call(f, 'k'); }, V, W);
  fr.remove();
  await row('worker DOMRect.toJSON / encodeInto', function () {
    var src = "var P = function (o) { var p = Object.getPrototypeOf(o); return p === Object.prototype ? 'Object.prototype' : p === Array.prototype ? 'Array.prototype' : p === null ? 'NULL' : 'other'; };" +
      "postMessage([P(new DOMRect(1, 2, 3, 4).toJSON()), P(new DOMPoint().toJSON()), P(new DOMMatrix().toJSON()), P(new TextEncoder().encodeInto('a', new Uint8Array(1))), P(new Headers().getSetCookie()), Object.keys(new DOMRect().toJSON()).join(',')]);";
    var w = new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
    return new Promise(function (res) { w.onmessage = function (e) { w.terminate(); res(e.data); }; w.onerror = function (e) { res(['worker error', String(e.message)]); }; });
  }, V);
  return out;
})()
