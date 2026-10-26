(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = ['window.open'];
  const R = await ((async () => {
  const R = [];
  const t = async (name, f) => { try { R.push([name, await f()]); } catch (e) { R.push([name, e.name + ': ' + e.message]); } };
  const gap = async (f) => { const a = setTimeout(() => {}, 0); await f(); const b = setTimeout(() => {}, 0); clearTimeout(a); clearTimeout(b); return b - a; };
  await t('nothing', () => gap(() => {}));
  await t('AbortSignal.timeout', () => gap(() => { AbortSignal.timeout(10); }));
  await t('scheduler.postTask delay', () => gap(() => { if (self.scheduler) scheduler.postTask(() => {}, { delay: 5 }); }));
  await t('scheduler.postTask', () => gap(() => { if (self.scheduler) scheduler.postTask(() => {}); }));
  await t('scheduler.yield', () => gap(async () => { if (self.scheduler && scheduler.yield) await scheduler.yield(); }));
  await t('webgl loseContext', () => gap(() => { const c = document.createElement('canvas'); const g = c.getContext('webgl'); if (g) { const x = g.getExtension('WEBGL_lose_context'); if (x) x.loseContext(); } }));
  await t('webgl2 loseContext', () => gap(() => { const c = document.createElement('canvas'); const g = c.getContext('webgl2'); if (g) { const x = g.getExtension('WEBGL_lose_context'); if (x) x.loseContext(); } }));
  await t('requestIdleCallback', () => gap(() => { requestIdleCallback(() => {}); }));
  await t('requestAnimationFrame', () => gap(() => { requestAnimationFrame(() => {}); }));
  await t('queueMicrotask', () => gap(() => { queueMicrotask(() => {}); }));
  await t('postMessage', () => gap(() => { postMessage('x', '*'); }));
  await t('fetch data', () => gap(() => fetch('data:,x').then((r) => r.text())));
  await t('iframe srcdoc', () => gap(() => { const f = document.createElement('iframe'); f.srcdoc = '<p>x'; document.body.appendChild(f); }));
  await t('img decode', () => gap(() => { const i = new Image(); i.src = 'data:image/gif;base64,R0lGODlhAQABAAAAACw='; return i.decode().catch(() => 0); }));
  await t('chrome.csi', () => gap(() => { if (self.chrome && chrome.csi) chrome.csi(); }));
  await t('chrome.loadTimes', () => gap(() => { if (self.chrome && chrome.loadTimes) chrome.loadTimes(); }));
  await t('document.startViewTransition', () => gap(async () => { if (document.startViewTransition) { const v = document.startViewTransition(() => {}); await v.finished.catch(() => 0); } }));
  await t('window.open', () => gap(() => { try { window.open('about:blank', '_blank', 'noopener'); } catch (e) {} }));
  await t('setInterval cleared', () => gap(() => { const i = setInterval(() => {}, 100); clearInterval(i); }));
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
