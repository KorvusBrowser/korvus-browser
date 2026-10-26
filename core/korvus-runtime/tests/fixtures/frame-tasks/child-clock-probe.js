(async () => {
  const r = [];
  await new Promise((res) => setTimeout(res, 300));
  const f = document.createElement('iframe');
  document.body.appendChild(f);
  const cw = f.contentWindow;
  await new Promise((res) => setTimeout(res, 200));
  const d = cw.performance.timeOrigin - performance.timeOrigin;
  r.push(['child-origin-later', d > 250, d < 2000]);
  r.push(['child-now-small', cw.performance.now() < 500]);
  r.push(['nav-entries', cw.performance.getEntriesByType('navigation').length]);
  return r;
})()
