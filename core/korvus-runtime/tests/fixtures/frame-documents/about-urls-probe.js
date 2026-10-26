(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = [];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const t = async (name, f) => { try { R.push([name, await f()]); } catch (e) { R.push([name, e.name + ': ' + e.message]); } };
  const ts = (o) => Object.prototype.toString.call(o);
  await t('modelContext in navigator', () => ['modelContext' in navigator, typeof navigator.modelContext, ts(navigator.modelContext), typeof self.ModelContext]);
  await t('battery', async () => { const b = await navigator.getBattery(); const p = Object.getPrototypeOf(b);
    return [ts(b), b.charging, b.level, b.chargingTime, String(b.dischargingTime), Object.getOwnPropertyNames(b).join(','), p === (self.BatteryManager && BatteryManager.prototype), Object.getOwnPropertyNames(p).join(','), typeof Object.getOwnPropertyDescriptor(p, 'level').get, b === await navigator.getBattery()]; });
  await t('srcdoc frame', () => new Promise((res) => { const f = document.createElement('iframe'); f.srcdoc = '<p>x</p>'; f.onload = () => { const w = f.contentWindow, d = f.contentDocument; res([w.location.href, d.URL, d.documentURI, d.baseURI, w.origin, d.referrer, w.location.origin, String(w.location), d.compatMode, d.characterSet, d.contentType]); }; document.body.appendChild(f); }));
  await t('srcdoc frame sync', () => { const f = document.createElement('iframe'); f.srcdoc = '<p>x</p>'; document.body.appendChild(f); const w = f.contentWindow, d = f.contentDocument; return [w.location.href, d.URL, d.readyState, d.body ? d.body.innerHTML : null]; });
  await t('blank frame', () => { const f = document.createElement('iframe'); document.body.appendChild(f); const w = f.contentWindow, d = f.contentDocument; return [w.location.href, d.URL, d.baseURI, w.origin, d.referrer]; });
  const gone = () => { const f = document.createElement('iframe'); document.body.appendChild(f); const d = f.contentDocument; const w = f.contentWindow; const b = d.body; f.remove(); return { d, w, b }; };
  await t('removed frame cs', () => { const { b } = gone(); const cs = getComputedStyle(b); return [cs.length, cs.cssText, cs.display, cs.getPropertyValue('display'), cs.item(0), cs[0], cs.parentRule, cs.getPropertyPriority('display'), cs.color, cs.width, cs.cssFloat, Object.keys(cs).length]; });
  await t('removed frame cs pseudo', () => { const { b } = gone(); const cs = getComputedStyle(b, '::before'); return [cs.length, cs.content, cs.display]; });
  await t('removed frame own window cs', () => { const f = document.createElement('iframe'); document.body.appendChild(f); const w = f.contentWindow, b = f.contentDocument.body; f.remove(); let r; try { const cs = w.getComputedStyle(b); r = [cs.length, cs.display]; } catch (e) { r = e.name + ': ' + e.message; } return r; });
  await t('cs taken before removal', () => { const f = document.createElement('iframe'); document.body.appendChild(f); const b = f.contentDocument.body; const cs = getComputedStyle(b); const before = [cs.length, cs.display]; f.remove(); return before.concat([cs.length, cs.display, cs[0]]); });
  await t('display none frame cs', () => { const f = document.createElement('iframe'); f.style.display = 'none'; document.body.appendChild(f); const cs = getComputedStyle(f.contentDocument.body); return [cs.length, cs.display, cs.width]; });
  await t('createDocument element cs', () => { const d = document.implementation.createHTMLDocument('x'); const e = d.createElement('div'); d.body.appendChild(e); const cs = getComputedStyle(e); return [cs.length, cs.display, cs.width, cs.color]; });
  await t('DOMParser doc cs', () => { const d = new DOMParser().parseFromString('<p style="color:red">x</p>', 'text/html'); const cs = getComputedStyle(d.querySelector('p')); return [cs.length, cs.display, cs.color]; });
  await t('template content cs', () => { const tp = document.createElement('template'); tp.innerHTML = '<div>x</div>'; const cs = getComputedStyle(tp.content.firstChild); return [cs.length, cs.display]; });
  await t('detached cs', () => { const e = document.createElement('div'); const cs = getComputedStyle(e); return [cs.length, cs.display, cs.width, cs.color]; });
  await t('not element', () => { try { getComputedStyle(document); return 'ok'; } catch (e) { return e.name + ': ' + e.message; } });
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
