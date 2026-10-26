(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = [];
  const SKIP_PREFIX = [];
  const R = await ((() => {
  const R = [];
  const t = (name, f) => { try { R.push([name, f()]); } catch (e) { R.push([name, e.name + ': ' + e.message]); } };
  const describe = (cs) => {
    let forin = 0, numeric = 0, first = null, firstNamed = null;
    for (const k in cs) { forin++; if (/^\d+$/.test(k)) numeric++; else if (firstNamed === null) firstNamed = k; if (first === null) first = k; }
    const keys = Object.keys(cs);
    return [cs.length, keys.length, forin, numeric, first, firstNamed, cs[0], cs[cs.length - 1], cs.display, 'accentColor' in cs, Object.getOwnPropertyNames(cs).length];
  };
  const mk = (host, styleText) => {
    const f = document.createElement('iframe');
    f.height = 1; f.width = 1;
    if (styleText) f.style.cssText = styleText;
    host.appendChild(f);
    return f;
  };
  t('main body', () => describe(getComputedStyle(document.body)));
  t('main body via its view', () => describe(document.defaultView.getComputedStyle(document.body)));
  t('frame in body', () => { const f = mk(document.body); return describe(getComputedStyle(f.contentDocument.body)); });
  t('frame in body, its own view', () => { const f = mk(document.body); return describe(f.contentWindow.getComputedStyle(f.contentDocument.body)); });
  t('frame in closed shadow', () => { const h = document.createElement('div'); document.body.appendChild(h); const s = h.attachShadow({ mode: 'closed' }); const f = mk(s); return describe(getComputedStyle(f.contentDocument.body)); });
  t('frame display none', () => { const f = mk(document.body, 'display:none'); return describe(getComputedStyle(f.contentDocument.body)); });
  t('frame under display none', () => { const h = document.createElement('div'); h.style.display = 'none'; document.body.appendChild(h); const f = mk(h); return describe(getComputedStyle(f.contentDocument.body)); });
  t('frame visibility hidden', () => { const f = mk(document.body, 'visibility:hidden;position:absolute'); return describe(getComputedStyle(f.contentDocument.body)); });
  t('frame removed', () => { const f = mk(document.body); const d = f.contentDocument; f.remove(); return describe(getComputedStyle(d.body)); });
  t('detached element', () => describe(getComputedStyle(document.createElement('div'))));
  t('frame documentElement', () => { const f = mk(document.body); return describe(getComputedStyle(f.contentDocument.documentElement)); });
  t('frame doc written', () => { const f = mk(document.body); const d = f.contentDocument; d.open(); d.write('<html><head></head><body></body></html>'); d.close(); return describe(getComputedStyle(d.body)); });
  t('createHTMLDocument body', () => { const d = document.implementation.createHTMLDocument(''); return describe(getComputedStyle(d.body)); });
  t('names sample', () => { const f = mk(document.body); const cs = getComputedStyle(f.contentDocument.body); const a = []; for (let i = 0; i < cs.length; i += 37) a.push(cs[i]); return a.join(','); });
  t('named keys sample', () => { const cs = getComputedStyle(document.body); const a = []; for (const k in cs) if (!/^\d+$/.test(k)) a.push(k); return [a.length, a.slice(0, 5).join(','), a.slice(-5).join(',')]; });
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
