(async () => {
  // Member conversions the sweep and the values probe do not reach: an integer member's [Clamp] and [EnforceRange],
  // a Symbol or a BigInt given where a string or a number is converted, the two DOMException constructors that take
  // a dictionary (Blink 150's WebTransportError takes ONE, `WebTransportErrorInit`), and the members a WORKER reads
  // (an [Exposed=Window] member is read in a window only). Rows: [label, outcome, detail].
  const out = [];
  const val = (v) => (v === undefined ? 'undefined' : (typeof v === 'number' && isNaN(v) ? 'NaN' : v));
  const t = (label, f) => {
    try { out.push([label, 'ok', val(f())]); } catch (e) { out.push([label, e.name, e.message]); }
  };
  const ex = (e) => JSON.stringify([e.name, e.code, e.message]);
  // [Clamp] octet: WebTransportErrorInit.streamErrorCode
  for (const x of [300, 255.5, 254.5, 2.5, 3.5, -2.5, -3, NaN, Infinity, -Infinity, '7.5', null, 1e12]) {
    t('wterr/' + String(x), () => { const e = new WebTransportError({streamErrorCode: x}); return JSON.stringify([e.streamErrorCode, e.source]); });
  }
  t('wterr/none', () => { const e = new WebTransportError(); return JSON.stringify([e.streamErrorCode, e.source]) + ex(e); });
  t('wterr/message', () => ex(new WebTransportError({message: 'hi'})));
  t('wterr/string', () => new WebTransportError('m'));
  t('wterr/two', () => { const e = new WebTransportError({message: 'a'}, {streamErrorCode: 4}); return JSON.stringify([e.message, e.streamErrorCode]); });
  // [Clamp] unsigned short: WebSocketCloseInfo.closeCode, then the close code and reason checks
  for (const x of [1000, 3000.5, 3001.5, 4999.5, 70000, -1, NaN, 1000.4, 2999]) {
    t('wserr/' + String(x), () => { const e = new WebSocketError('m', {closeCode: x}); return JSON.stringify([e.closeCode, e.reason]); });
  }
  t('wserr/none', () => { const e = new WebSocketError(); return JSON.stringify([e.closeCode, e.reason]) + ex(e); });
  t('wserr/reason-only', () => { const e = new WebSocketError('m', {reason: 'r'}); return JSON.stringify([e.closeCode, e.reason]) + ex(e); });
  t('wserr/reason-123', () => new WebSocketError('m', {closeCode: 1000, reason: 'x'.repeat(123)}).reason.length);
  t('wserr/reason-124', () => new WebSocketError('m', {closeCode: 1000, reason: 'x'.repeat(124)}));
  t('wserr/reason-utf8', () => new WebSocketError('m', {reason: 'é'.repeat(62)}));
  // [EnforceRange] unsigned short: RTCDataChannelInit.id
  const pc = new RTCPeerConnection();
  for (const x of [70000, -1, NaN, Infinity, -Infinity, 5.9, '12']) {
    t('dcid/' + String(x), () => pc.createDataChannel('c', {negotiated: true, id: x}).id);
  }
  pc.close();
  // [EnforceRange] unsigned long long: SchedulerPostTaskOptions.delay (a promise operation: it rejects)
  for (const x of [-1, 2 ** 53, Infinity]) {
    const r = await Promise.race([scheduler.postTask(() => 'ran', {delay: x}).then((v) => 'resolved ' + v, (e) => e.name + ': ' + e.message),
      new Promise((res) => setTimeout(() => res('pending'), 100))]);
    out.push(['delay/' + String(x), 'ok', r]);
  }
  // an interface-typed member takes a platform object of ANOTHER realm: a frame's AbortSignal
  const fr = document.createElement('iframe');
  document.body.appendChild(fr);
  const fac = new fr.contentWindow.AbortController();
  out.push(['xrealm/postTask-signal', 'ok', await scheduler.postTask(() => 'ran', {signal: fac.signal}).then((v) => v, (e) => e.name + ': ' + e.message)]);
  t('xrealm/listener-signal', () => { let n = 0; const d = document.createElement('div'); d.addEventListener('z', () => n++, {signal: fac.signal}); fac.abort(); d.dispatchEvent(new Event('z')); return n; });
  fr.remove();
  // a Symbol or a BigInt where a string or a number is converted: V8's refusal, with Blink's context in front
  t('sym/worker-name', () => new Worker('data:,', {name: Symbol('s')}));
  t('sym/blob-type', () => new Blob([], {type: Symbol('s')}));
  t('sym/blob-endings', () => new Blob([], {endings: Symbol('s')}));
  t('sym/mouse-clientX', () => new MouseEvent('x', {clientX: Symbol('s')}));
  t('big/mouse-clientX', () => new MouseEvent('x', {clientX: 1n}));
  t('big/progress-loaded', () => new ProgressEvent('x', {loaded: 1n}));
  t('big/mouse-button', () => new MouseEvent('x', {button: 1n}));
  // the members a worker reads: RequestInit's [Exposed=Window] members are not
  const src = "const r = []; const n = (k) => (typeof k === 'symbol' ? '@' + k.description : String(k));" +
    " const p = new Proxy({}, {get(o, k) { r.push(n(k)); return undefined; }, has(o, k) { r.push('has:' + n(k)); return false; }});" +
    " try { new Request('https://a.test/', p); } catch (e) { r.push('E ' + e.message); } postMessage(r.join());";
  const w = new Worker(URL.createObjectURL(new Blob([src])));
  const reads = await new Promise((res) => { w.onmessage = (e) => res(e.data); setTimeout(() => res('timeout'), 5000); });
  w.terminate();
  out.push(['worker/request-reads', 'ok', reads]);
  return out;
})()
