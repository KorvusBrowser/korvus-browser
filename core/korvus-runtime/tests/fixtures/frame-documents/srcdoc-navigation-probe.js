(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = [];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const t = async (name, f) => { try { R.push([name, await f()]); } catch (e) { R.push([name, e.name + ': ' + e.message]); } };
  const strip = (s) => typeof s === 'string' ? s.replace(/:\d+/g, ':P') : s;
  const loaded = (f) => new Promise((r) => f.addEventListener('load', () => r(), { once: true }));
  self.order = [];
  await t('sync view', () => { const f = document.createElement('iframe'); f.srcdoc = '<p>x</p>'; document.body.appendChild(f); const w = f.contentWindow, d = f.contentDocument; return [w.location.href, d.URL, d.readyState, d.body ? d.body.innerHTML : null, d.documentElement ? d.documentElement.outerHTML : null, strip(d.baseURI)]; });
  await t('order', async () => { order.length = 0; const f = document.createElement('iframe'); f.srcdoc = '<script>parent.order.push("inner script " + document.readyState)<\/script>'; f.addEventListener('load', () => order.push('iframe load')); document.body.appendChild(f); order.push('after append'); Promise.resolve().then(() => order.push('micro')); setTimeout(() => order.push('timeout0'), 0); await new Promise((r) => setTimeout(r, 300)); return order.join(' | '); });
  await t('window reuse', async () => { const f = document.createElement('iframe'); f.srcdoc = '<p>y</p>'; document.body.appendChild(f); const w0 = f.contentWindow; const d0 = f.contentDocument; w0.marker = 42; await loaded(f); return [f.contentWindow === w0, f.contentWindow.marker, f.contentDocument === d0, f.contentDocument.body.innerHTML]; });
  await t('base URL after load', async () => { const f = document.createElement('iframe'); f.srcdoc = '<a href="rel">a</a>'; document.body.appendChild(f); await loaded(f); const d = f.contentDocument; return [strip(d.baseURI), strip(d.querySelector('a').href), d.URL, strip(d.referrer), f.contentWindow.origin === self.origin]; });
  await t('base element in parent', async () => { const base = document.createElement('base'); base.href = '/sub/dir/'; document.head.appendChild(base); const f = document.createElement('iframe'); f.srcdoc = '<a href="rel">a</a>'; document.body.appendChild(f); await loaded(f); const r = [strip(f.contentDocument.baseURI), strip(f.contentDocument.querySelector('a').href)]; const g = document.createElement('iframe'); document.body.appendChild(g); r.push(strip(g.contentDocument.baseURI)); base.remove(); r.push(strip(g.contentDocument.baseURI), strip(f.contentDocument.baseURI)); return r; });
  await t('blank frame base', () => { const g = document.createElement('iframe'); document.body.appendChild(g); const d = g.contentDocument; const a = d.createElement('a'); a.href = 'rel?q'; return [strip(d.baseURI), strip(a.href), strip(d.referrer)]; });
  await t('srcdoc change', async () => { const f = document.createElement('iframe'); f.srcdoc = '<p>1</p>'; document.body.appendChild(f); await loaded(f); f.srcdoc = '<p>2</p>'; const sync = f.contentDocument.body.innerHTML; await loaded(f); return [sync, f.contentDocument.body.innerHTML]; });
  await t('srcdoc beats src', async () => { const f = document.createElement('iframe'); f.src = 'about:blank#x'; f.srcdoc = '<p>s</p>'; document.body.appendChild(f); await loaded(f); return [f.contentWindow.location.href, f.contentDocument.body.innerHTML]; });
  await t('remove srcdoc attr', async () => { const f = document.createElement('iframe'); f.srcdoc = '<p>s</p>'; document.body.appendChild(f); await loaded(f); f.removeAttribute('srcdoc'); await Promise.race([loaded(f), new Promise((r) => setTimeout(r, 500))]); return [f.contentWindow.location.href, f.contentDocument.body ? f.contentDocument.body.innerHTML : null]; });
  await t('load count', async () => { let n = 0; const f = document.createElement('iframe'); f.srcdoc = '<p>c</p>'; f.onload = () => n++; document.body.appendChild(f); await new Promise((r) => setTimeout(r, 300)); return n; });
  await t('blank load count', async () => { let n = 0; const f = document.createElement('iframe'); f.onload = () => n++; document.body.appendChild(f); const sync = n; await new Promise((r) => setTimeout(r, 300)); return [sync, n]; });
  await t('srcdoc compat', async () => { const f = document.createElement('iframe'); f.srcdoc = '<p>q</p>'; document.body.appendChild(f); await loaded(f); return [f.contentDocument.compatMode, f.contentDocument.doctype === null]; });
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN').split('http://127.0.0.1:P').join('ORIGIN'));
})()
