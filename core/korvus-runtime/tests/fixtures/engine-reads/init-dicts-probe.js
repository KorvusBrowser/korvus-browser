(async () => {
  const R = [];
  const hits = [];
  const names = ['bubbles', 'cancelable', 'composed', 'data', 'origin', 'lastEventId', 'source', 'ports', 'loaded', 'total', 'lengthComputable', 'detail', 'oldState', 'newState', 'isTrusted', 'view', 'relatedTarget', 'message', 'filename', 'lineno', 'colno', 'error', 'reason', 'promise', 'state', 'newURL', 'oldURL', 'persisted', 'inputType', 'key', 'code', 'candidate', 'track', 'streams'];
  for (const n of names) Object.defineProperty(Object.prototype, n, { get() { hits.push(n); return undefined; }, configurable: true });
  const take = () => { const h = [...new Set(hits)].sort().join(','); hits.length = 0; return h; };
  take();
  await new Promise((r) => { const x = new XMLHttpRequest(); x.onloadend = r; x.open('GET', '/nothing'); x.send(); });
  R.push(['xhr', take()]);
  await new Promise((r) => { addEventListener('message', r, { once: true }); postMessage('m', '*'); });
  R.push(['postMessage', take()]);
  const cb = document.createElement('input'); cb.type = 'checkbox'; document.body.appendChild(cb); cb.click();
  R.push(['checkbox click', take()]);
  await new Promise((r) => { const mc = new MessageChannel(); mc.port2.onmessage = r; mc.port1.postMessage(1); });
  R.push(['MessageChannel', take()]);
  history.pushState(null, '', '#q'); await new Promise((r) => { addEventListener('popstate', r, { once: true }); history.back(); setTimeout(r, 300); });
  R.push(['popstate', take()]);
  location.hash = 'z'; await new Promise((r) => setTimeout(r, 100));
  R.push(['hashchange', take()]);
  const ta = document.createElement('textarea'); document.body.appendChild(ta); ta.focus(); ta.blur();
  R.push(['focus blur', take()]);
  await new Promise((r) => { const fr = new FileReader(); fr.onloadend = r; fr.readAsText(new Blob(['x'])); });
  R.push(['FileReader', take()]);
  await new Promise((r) => { const i = document.createElement('iframe'); i.onload = r; document.body.appendChild(i); });
  R.push(['iframe load', take()]);
  for (const n of names) delete Object.prototype[n];
  return R;
})()
