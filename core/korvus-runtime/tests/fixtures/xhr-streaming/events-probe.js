(async () => {
  // Every event an XMLHttpRequest fires while a local server trickles its response in parts 300 ms apart: the
  // type, the readyState, the progress numbers, responseText's length (or what it throws), what `response` is,
  // and status. A `peek` row reads the request from a timer between two parts.
  const R = [];
  const T = (o) => Object.prototype.toString.call(o);
  const run = (path, opts = {}) => new Promise((resolve) => {
    const x = new XMLHttpRequest();
    const ev = [];
    const rec = (e) => {
      let tl; try { tl = x.responseText.length; } catch (err) { tl = err.name; }
      let resp; try { resp = x.response === null ? 'null' : typeof x.response === 'string' ? 'text ' + x.response.length : T(x.response); } catch (err) { resp = err.name; }
      const row = [e.type, x.readyState];
      if (e.type !== 'readystatechange') row.push(e.loaded, e.total, e.lengthComputable);
      row.push(tl, resp, x.status);
      ev.push(row);
      if (opts.onEvent) opts.onEvent(x, e);
      if (e.type === 'loadend' && !opts.keep) setTimeout(() => resolve(ev), 200);
    };
    for (const ty of ['readystatechange', 'loadstart', 'progress', 'load', 'loadend', 'abort', 'error', 'timeout']) x.addEventListener(ty, rec);
    x.open('GET', path);
    if (opts.type) x.responseType = opts.type;
    if (opts.timeout) x.timeout = opts.timeout;
    x.send();
    if (opts.peek) setTimeout(() => ev.push(['peek', x.readyState, x.status, x.statusText, x.getAllResponseHeaders(), x.getResponseHeader('content-type'), x.responseText.length]), opts.peek);
    if (opts.keep) setTimeout(() => resolve(ev), opts.keep);
  });
  const P = '/p?ct=text/plain;charset=utf-8&d=';
  const H = '/p?ct=text/html;charset=utf-8&d=';
  // readyState stays OPENED -- status 0, no headers -- until the first body bytes are delivered, not when the headers arrive
  R.push(['text/html, 2 x 100 bytes', await run(H + '300:100,300:100', {peek: 150})]);
  // the network service holds a sniffable type's first bytes until it can decide: 1024 for text/plain
  R.push(['text/plain, 3 x 1000 bytes', await run(P + '300:1000,300:1000,300:1000', {peek: 450})]);
  R.push(['text/plain chunked', await run(P + '300:1000,300:1000,300:1000&cl=0')]);
  R.push(['text/plain nosniff', await run(P + '300:1000,300:1000&nosniff=1')]);
  R.push(['text/plain, a binary byte first', await run(P + '300:bin,300:1000')]);
  R.push(['text/plain, a BOM first', await run(P + '300:bom,300:1000')]);
  R.push(['no type, html', await run('/p?d=300:html,300:1000')]);
  R.push(['no type, text', await run('/p?d=300:100,300:1000')]);
  R.push(['octet-stream', await run('/p?ct=application/octet-stream&d=300:100,300:1000')]);
  R.push(['text/xml, a root tag', await run('/p?ct=text/xml&d=300:xmlroot,300:100')]);
  R.push(['text/xml, a declaration only', await run('/p?ct=text/xml&d=300:xmldecl,300:300')]);
  R.push(['application/json', await run('/p?ct=application/json&d=300:ja,300:jb', {type: 'json'})]);
  // a character split across two deliveries is not in responseText until it is whole
  R.push(['UTF-8 split', await run(H + '300:u8a,300:u8b')]);
  // the last bytes inside the 50 ms window: their progress is flushed at the end, in DONE
  R.push(['two parts 30 ms apart, then the end', await run(H + '300:100,30:100')]);
  R.push(['one part', await run(P + '0:1000')]);
  R.push(['empty 200', await run(P)]);
  R.push(['empty chunked', await run(P + '&cl=0')]);
  R.push(['204', await run('/nocontent')]);
  // a coded body has no expected length
  R.push(['gzip', await run('/p?ct=application/json&gz=1&d=0:3000')]);
  R.push(['gzip, larger coded than decoded', await run('/p?ct=application/json&gz=1&gz0=1&d=0:rnd')]);
  R.push(['arraybuffer', await run(H + '300:1000,300:1000', {type: 'arraybuffer'})]);
  R.push(['blob', await run(H + '300:1000,300:1000', {type: 'blob'})]);
  R.push(['document', await run(H + '300:html,300:100', {type: 'document'})]);
  // what a handler does mid-flight
  R.push(['abort in readystatechange 2', await run(H + '300:1000,300:1000', {keep: 1000, onEvent: (x, e) => { if (e.type === 'readystatechange' && x.readyState === 2 && !x.__d) { x.__d = 1; x.abort(); } }})]);
  R.push(['abort in readystatechange 3', await run(H + '300:1000,300:1000', {keep: 1000, onEvent: (x, e) => { if (e.type === 'readystatechange' && x.readyState === 3 && !x.__d) { x.__d = 1; x.abort(); } }})]);
  R.push(['abort in the first progress', await run(H + '300:1000,300:1000,300:1000', {onEvent: (x, e) => { if (e.type === 'progress' && !x.__d) { x.__d = 1; x.abort(); } }})]);
  R.push(['abort in readystatechange 4', await run(H + '300:1000', {keep: 800, onEvent: (x, e) => { if (e.type === 'readystatechange' && x.readyState === 4 && !x.__d) { x.__d = 1; x.abort(); } }})]);
  R.push(['open in readystatechange 3', await run(H + '300:1000,300:1000', {keep: 1000, onEvent: (x, e) => { if (e.type === 'readystatechange' && x.readyState === 3 && !x.__d) { x.__d = 1; x.open('GET', '/p?d=0:1'); } }})]);
  R.push(['open in load', await run(H + '300:1000', {keep: 800, onEvent: (x, e) => { if (e.type === 'load' && !x.__d) { x.__d = 1; x.open('GET', '/p?d=0:1'); } }})]);
  R.push(['timeout mid-body', await run(H + '300:1000,300:1000,300:1000', {timeout: 750})]);
  R.push(['timeout while sniffing', await run(P + '300:1000,300:1000', {timeout: 450})]);
  // a body trickled for a second: progress at most every 50 ms, each after LOADING's first preceded by a readystatechange
  const K = await new Promise((resolve) => {
    const x = new XMLHttpRequest(); const ev = []; let t0 = 0;
    const rec = (e) => { ev.push([e.type, x.readyState, performance.now() - t0, e.loaded, e.total]); if (e.type === 'loadend') resolve(ev); };
    for (const ty of ['readystatechange', 'progress', 'load', 'loadend']) x.addEventListener(ty, rec);
    x.open('GET', H + Array.from({length: 40}, (_, i) => (i ? 25 : 300) + ':250').join(','));
    t0 = performance.now(); x.send();
  });
  const prog = K.filter((e) => e[0] === 'progress');
  const loading = prog.filter((e) => e[1] === 3);
  const gaps = loading.slice(1).map((e, i) => e[2] - loading[i][2]);
  const pairs = K.map((e, i) => [e, K[i + 1]]).filter(([e]) => e[0] === 'readystatechange' && e[1] === 3);
  R.push(['trickled for a second', [
    'progress events in LOADING > 10', loading.length > 10,
    'none closer than 40 ms', gaps.every((g) => g >= 40),
    'a readystatechange(3) per LOADING progress', pairs.length === loading.length,
    'each right before its progress', pairs.every(([, n]) => n && n[0] === 'progress' && n[1] === 3),
    'loaded only grows', prog.every((e, i) => !i || e[3] >= prog[i - 1][3]),
    'last progress', prog[prog.length - 1].slice(3),
    'ends', K.slice(-3).map((e) => [e[0], e[1]]),
  ]]);
  return R;
})()
