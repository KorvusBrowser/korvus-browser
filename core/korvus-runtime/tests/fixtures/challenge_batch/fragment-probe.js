(async () => {
  if (window !== top) return [];
  const out = [];
  const f = document.createElement('iframe');
  document.body.appendChild(f);
  await new Promise(r => setTimeout(r, 50));
  const w = f.contentWindow;
  out.push(['blank-href', w.location.href]);
  out.push(['blank-hash', w.eval('(location.hash = "x", location.hash + "|" + location.href)')]);
  let fired = 0; w.addEventListener('hashchange', () => fired++);
  w.location.hash = 'y';
  await new Promise(r => setTimeout(r, 50));
  out.push(['blank-hashchange', String(fired), w.location.href]);
  const f2 = document.createElement('iframe'); f2.srcdoc = '<p>x</p>';
  await new Promise(r => { f2.onload = r; document.body.appendChild(f2); });
  out.push(['srcdoc-hash', f2.contentWindow.eval('(location.hash = "z", location.hash + "|" + location.href)')]);
  out.push(['top-hash', (location.hash = 'q', location.hash)]);
  let topFired = 0; addEventListener('hashchange', () => topFired++);
  location.hash = 'r';
  await new Promise(r => setTimeout(r, 50));
  out.push(['top-hashchange', String(topFired)]);
  f.remove(); f2.remove();
  return out;
})()
