(async () => {
  const R = [];
  const log = [];
  const Orig = URL;
  const P = Orig.prototype;
  // wrap the global constructor and every URL.prototype accessor
  const Spy = new Proxy(Orig, { construct(t, a, nt) { log.push('new URL(' + a.map(String).join(',').slice(0, 40) + ')'); return Reflect.construct(t, a, nt); } });
  window.URL = Spy;
  for (const k of Object.getOwnPropertyNames(P)) {
    const d = Object.getOwnPropertyDescriptor(P, k);
    if (d && (d.get || d.set)) {
      Object.defineProperty(P, k, { configurable: true, enumerable: d.enumerable,
        get: d.get && function () { log.push('get ' + k); return d.get.call(this); },
        set: d.set && function (v) { log.push('set ' + k); return d.set.call(this, v); } });
    }
  }
  const step = async (name, f) => { log.length = 0; try { await f(); } catch (e) { log.push('threw ' + e.name); } R.push([name, JSON.stringify(log.slice())]); };
  const f = document.createElement('iframe');
  await step('append frame', async () => { document.body.appendChild(f); await new Promise(r => setTimeout(r, 50)); });
  await step('frame postMessage *', () => f.contentWindow.postMessage('x', '*'));
  await step('frame postMessage origin', () => f.contentWindow.postMessage('x', location.origin));
  await step('frame postMessage bad origin', () => f.contentWindow.postMessage('x', 'https://example.com'));
  await step('self postMessage origin', () => window.postMessage('x', location.origin));
  await step('frame location href', () => f.contentWindow.location.href);
  await step('frame location origin', () => f.contentWindow.location.origin);
  await step('frame location hash set', () => { f.contentWindow.location.hash = 'y'; });
  await step('frame location hash read', () => f.contentWindow.location.hash);
  const f2 = document.createElement('iframe'); f2.src = '/x?y=1';
  await step('frame with src', async () => { document.body.appendChild(f2); await new Promise(r => setTimeout(r, 100)); });
  await step('frame2 contentWindow', () => f2.contentWindow && f2.contentWindow.location.href);
  const f3 = document.createElement('iframe'); f3.src = 'http://localhost:' + location.port + '/z';
  await step('xo frame append', async () => { document.body.appendChild(f3); await new Promise(r => setTimeout(r, 300)); });
  await step('xo postMessage *', () => f3.contentWindow.postMessage('x', '*'));
  await step('xo postMessage its origin', () => f3.contentWindow.postMessage('x', 'http://localhost:' + location.port));
  await step('xo postMessage other origin', () => f3.contentWindow.postMessage('x', location.origin));
  await step('xo postMessage bad', () => f3.contentWindow.postMessage('x', 'nope'));
  await step('window.origin', () => window.origin);
  window.URL = Orig;
  return R;
})()
