(() => {
  const r = [];
  const f = document.createElement('iframe');
  document.body.appendChild(f);
  const w = f.contentWindow, d = f.contentDocument;
  r.push(['domain', d.domain, location.hostname]);
  r.push(['origin', w.origin, d.location.href]);
  r.push(['compat', d.compatMode, d.scrollingElement && d.scrollingElement.nodeName]);
  let inner = null;
  window.addEventListener('x', () => { inner = [String(window.event && window.event.type), String(w.event)]; });
  window.dispatchEvent(new Event('x'));
  r.push(['event-own-listener', inner]);
  let inner2 = null;
  w.addEventListener('y', () => { inner2 = [String(w.event && w.event.type), String(window.event)]; });
  w.dispatchEvent(new w.Event('y'));
  r.push(['event-child-listener', inner2]);
  let inner3 = null;
  w.addEventListener('z', () => { inner3 = [String(w.event && w.event.type), String(window.event)]; });
  window.addEventListener('z2', () => { w.dispatchEvent(new w.Event('z')); });
  window.dispatchEvent(new Event('z2'));
  r.push(['event-nested', inner3]);
  r.push(['outside', String(window.event), String(w.event)]);
  return r;
})()
