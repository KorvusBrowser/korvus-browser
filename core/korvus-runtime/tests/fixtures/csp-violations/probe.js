(async function () {
  // What a request a document's Content-Security-Policy refuses does, and the `securitypolicyviolation` event it fires
  // (ABPROG batch 4, a bot-detection script D1). Each case runs in a srcdoc frame of its own whose <head> meta holds the policy; the
  // frame's script (nonce'd, so a script-src case can still run it) does one thing and logs, in order, what it saw:
  // `r:<outcome>` from the call, `ev:<every field>` from a document listener. A row is [case, log].
  const BLOCKED = 'http://127.0.0.1:1/csp-probe';
  const cases = [
    ['img-src none, detached img', "img-src 'none'", `
      const i = new Image(); i.onload = () => log('r:load'); i.onerror = () => log('r:error'); i.src = '${BLOCKED}/a.gif?q=1#frag'; log('r:set');`],
    ['img-src none, connected img', "img-src 'none'", `
      const i = document.createElement('img'); i.onerror = () => log('r:error'); document.documentElement.append(i); i.src = '${BLOCKED}/b.gif';`],
    ['img-src self, data: image', "img-src 'self'", `
      const i = new Image(); i.onload = () => log('r:load'); i.onerror = () => log('r:error'); i.src = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';`],
    ['default-src none, img', "default-src 'none'; script-src 'nonce-t'", `
      const i = new Image(); i.onerror = () => log('r:error'); i.src = '${BLOCKED}/c.gif';`],
    ['connect-src none, fetch', "connect-src 'none'", `
      try { const p = fetch('${BLOCKED}/f?x=1', { method: 'POST', body: 'x' }); log('r:called'); await p; log('r:ok'); } catch (e) { log('r:' + e.name + ': ' + e.message); }`],
    ['connect-src none, xhr', "connect-src 'none'", `
      const x = new XMLHttpRequest(); x.onerror = () => log('r:error ' + x.readyState + ' ' + x.status); x.onloadend = () => log('r:loadend');
      try { x.open('GET', '${BLOCKED}/x'); x.send(); log('r:sent'); } catch (e) { log('r:threw ' + e.name); }`],
    ['connect-src none, sync xhr', "connect-src 'none'", `
      const x = new XMLHttpRequest(); try { x.open('GET', '${BLOCKED}/sx', false); x.send(); log('r:sent ' + x.status); } catch (e) { log('r:threw ' + e.name + ': ' + e.message); }`],
    ['connect-src none, sendBeacon', "connect-src 'none'", `
      log('r:' + navigator.sendBeacon('${BLOCKED}/beacon', 'x'));`],
    ['connect-src none, EventSource', "connect-src 'none'", `
      try { const s = new EventSource('${BLOCKED}/es'); s.onerror = () => log('r:error ' + s.readyState); log('r:made'); } catch (e) { log('r:threw ' + e.name + ': ' + e.message); }`],
    ['connect-src none, WebSocket', "connect-src 'none'", `
      try { const s = new WebSocket('ws://127.0.0.1:1/ws'); s.onerror = () => log('r:error ' + s.readyState); s.onclose = (e) => log('r:close ' + e.code); log('r:made'); } catch (e) { log('r:threw ' + e.name + ': ' + e.message); }`],
    ['style-src none, link stylesheet', "style-src 'none'", `
      const l = document.createElement('link'); l.rel = 'stylesheet'; l.onload = () => log('r:load'); l.onerror = () => log('r:error'); l.href = '${BLOCKED}/s.css'; document.head.append(l);`],
    ['font-src none, FontFace', "font-src 'none'", `
      try { await new FontFace('pf', 'url(${BLOCKED}/f.woff2)').load(); log('r:loaded'); } catch (e) { log('r:' + e.name + ': ' + e.message); }`],
    ['media-src none, audio', "media-src 'none'", `
      const a = document.createElement('audio'); a.onerror = () => log('r:error ' + (a.error && a.error.code)); a.src = '${BLOCKED}/a.mp3'; log('r:set');`],
    ['frame-src none, iframe', "frame-src 'none'", `
      const f = document.createElement('iframe'); f.onload = () => log('r:load'); f.src = '${BLOCKED}/frame'; document.documentElement.append(f);`],
    ['worker-src none, Worker', "worker-src 'none'", `
      try { const w = new Worker(URL.createObjectURL(new Blob(['1'], { type: 'text/javascript' }))); w.onerror = () => log('r:error'); log('r:made'); } catch (e) { log('r:threw ' + e.name + ': ' + e.message); }`],
    ['script-src nonce, script element', "script-src 'nonce-t'", `
      const s = document.createElement('script'); s.onload = () => log('r:load'); s.onerror = () => log('r:error'); s.src = '${BLOCKED}/s.js'; document.head.append(s);`],
    ['script-src nonce, inline script', "script-src 'nonce-t'", `
      const s = document.createElement('script'); s.textContent = 'window.ran = 1; // inline body long enough to be cut at forty characters'; document.head.append(s); log('r:' + (window.ran || 0));`],
    ['script-src nonce, eval', "script-src 'nonce-t'", `
      try { eval('1 + 1 /* an eval sample long enough to be cut at forty characters */'); log('r:ran'); } catch (e) { log('r:' + e.name + ': ' + e.message); }`],
    ['img-src none, two policies', "img-src 'none', img-src *", `
      const i = new Image(); i.onerror = () => log('r:error'); i.src = '${BLOCKED}/d.gif';`],
  ];
  const fields = (e) => [
    'target=' + e.target.nodeName,
    e.violatedDirective, e.effectiveDirective, e.blockedURI, e.originalPolicy, e.disposition, 'sample=' + e.sample,
    'status=' + e.statusCode, 'file=' + e.sourceFile, 'line=' + e.lineNumber, 'col=' + e.columnNumber,
    'doc=' + e.documentURI, 'ref=' + e.referrer, e.bubbles, e.composed, e.cancelable, e.isTrusted,
  ].join('|');
  const out = [];
  for (const [name, policy, body] of cases) {
    const f = document.createElement('iframe');
    window.__fields = fields;
    f.srcdoc = '<!doctype html><head><meta http-equiv="Content-Security-Policy" content="' + policy.replace(/"/g, '&quot;') + '">' +
      '<script nonce="t">window.L = []; const log = (s) => L.push(s); const fields = parent.__fields;' +
      'document.addEventListener("securitypolicyviolation", (e) => log("ev:" + fields(e)));' +
      'window.addEventListener("securitypolicyviolation", () => log("win"));' +
      '(async () => {' + body + '})();<\/script></head><body></body>';
    document.documentElement.append(f);
    await new Promise((r) => setTimeout(r, 700));
    let log;
    try { log = f.contentWindow.L || ['no log']; } catch (e) { log = ['unreadable ' + e.name]; }
    out.push([name, log.join(' ; ')]);
    f.remove();
  }
  return out;
})()
