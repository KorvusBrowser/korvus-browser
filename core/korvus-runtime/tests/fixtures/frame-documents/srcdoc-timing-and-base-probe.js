(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = [];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  // srcdoc and about:blank frames as stock Chrome 150 keeps them (an interactive-widget challenge 2026-10-09): a srcdoc navigation commits a
  // few milliseconds after it begins (the frame shows its initial about:blank document until then), and an
  // about:blank / srcdoc document's base URL is its creator's, kept as it was when the frame was made.
  const R = [];
  const t = async (name, f) => { try { R.push([name, await f()]); } catch (e) { R.push([name, e.name + ': ' + e.message]); } };
  const loaded = (f) => new Promise((r) => f.addEventListener('load', () => r(), { once: true }));
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const rel = (u) => typeof u === 'string' ? u.replace(location.origin, 'ORIGIN') : u;
  self.order = [];
  await t('sync view', () => { const f = document.createElement('iframe'); f.srcdoc = '<p>x</p>'; document.body.appendChild(f); const w = f.contentWindow, d = f.contentDocument; return [w.location.href, d.URL, d.readyState, d.body ? d.body.innerHTML : null, d.documentElement ? d.documentElement.outerHTML : null]; });
  await t('order', async () => { order.length = 0; const f = document.createElement('iframe'); f.srcdoc = '<script>parent.order.push("inner script " + document.readyState)<\/script>'; f.addEventListener('load', () => order.push('iframe load')); document.body.appendChild(f); order.push('after append'); Promise.resolve().then(() => order.push('micro')); setTimeout(() => order.push('timeout0'), 0); await wait(200); return order.join(' | '); });
  await t('two frames', async () => { order.length = 0; const a = document.createElement('iframe'); a.srcdoc = '<script>parent.order.push("A")<\/script>'; const b = document.createElement('iframe'); b.srcdoc = '<script>parent.order.push("B")<\/script>'; document.body.appendChild(a); document.body.appendChild(b); setTimeout(() => order.push('t0'), 0); await wait(200); return order.join(' '); });
  await t('window reuse', async () => { const f = document.createElement('iframe'); f.srcdoc = '<p>y</p>'; document.body.appendChild(f); const w0 = f.contentWindow; const d0 = f.contentDocument; w0.marker = 42; await loaded(f); return [f.contentWindow === w0, f.contentWindow.marker, f.contentDocument === d0, f.contentDocument.body.innerHTML]; });
  await t('srcdoc change', async () => { const f = document.createElement('iframe'); f.srcdoc = '<p>1</p>'; document.body.appendChild(f); await loaded(f); f.srcdoc = '<p>2</p>'; const sync = f.contentDocument.body.innerHTML; await loaded(f); return [sync, f.contentDocument.body.innerHTML]; });
  await t('setAttribute srcdoc', async () => { const f = document.createElement('iframe'); f.srcdoc = '<p>1</p>'; document.body.appendChild(f); await loaded(f); f.setAttribute('srcdoc', '<p>3</p>'); const sync = f.contentDocument.body.innerHTML; await loaded(f); return [sync, f.contentDocument.body.innerHTML]; });
  await t('remove srcdoc attr', async () => { const f = document.createElement('iframe'); f.srcdoc = '<p>s</p>'; document.body.appendChild(f); await loaded(f); f.removeAttribute('srcdoc'); await Promise.race([loaded(f), wait(500)]); return [f.contentWindow.location.href, f.contentDocument.body ? f.contentDocument.body.innerHTML : null]; });
  await t('srcdoc beats src', async () => { const f = document.createElement('iframe'); f.src = 'about:blank#x'; f.srcdoc = '<p>s</p>'; document.body.appendChild(f); await loaded(f); return [f.contentWindow.location.href, f.contentDocument.body.innerHTML]; });
  await t('load count', async () => { let n = 0; const f = document.createElement('iframe'); f.srcdoc = '<p>c</p>'; f.onload = () => n++; document.body.appendChild(f); await wait(200); return n; });
  await t('blank frame base', () => { const g = document.createElement('iframe'); document.body.appendChild(g); const d = g.contentDocument; const a = d.createElement('a'); a.href = 'rel?q'; return [rel(d.baseURI), rel(a.href)]; });
  // a blank frame made by a srcdoc document with its own <base>: the creator's base, not the page's
  await t('nested creator base', async () => {
    const f = document.createElement('iframe'); f.srcdoc = '<base href="https://creator.example/dir/"><p>c</p>'; document.body.appendChild(f); await loaded(f);
    const d = f.contentDocument; const g = d.createElement('iframe'); d.body.appendChild(g); const gd = g.contentDocument;
    const a = gd.createElement('a'); a.href = 'x/y'; return [rel(d.baseURI), rel(gd.baseURI), rel(a.href)];
  });
  await t('base snapshot', async () => {
    const base = document.createElement('base'); base.href = '/sub/dir/'; document.head.appendChild(base);
    const f = document.createElement('iframe'); f.srcdoc = '<a href="rel">a</a>'; document.body.appendChild(f); await loaded(f);
    const g = document.createElement('iframe'); document.body.appendChild(g);
    const r = [rel(f.contentDocument.baseURI), rel(f.contentDocument.querySelector('a').href), rel(g.contentDocument.baseURI)];
    base.remove();
    r.push(rel(g.contentDocument.baseURI), rel(f.contentDocument.baseURI));
    return r;
  });
  // getComputedStyle of an element whose document's browsing context is gone
  await t('removed frame style', () => { const f = document.createElement('iframe'); document.body.appendChild(f); const b = f.contentDocument.body; const cs = getComputedStyle(b); const before = [cs.length > 0, cs.display]; f.remove(); return before.concat([cs.length, cs.display, cs.getPropertyValue('color'), cs.item(0)]); });
  await t('hidden frame style', () => { const f = document.createElement('iframe'); f.style.display = 'none'; document.body.appendChild(f); const cs = getComputedStyle(f.contentDocument.body); return [cs.length > 0, cs.display]; });
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
