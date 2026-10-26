(() => {
  const f = document.createElement('iframe');
  document.body.appendChild(f);
  const d = f.contentDocument;
  d.write('<html><head></head><body></body></html>');
  d.close();
  const cs = getComputedStyle(d.body);
  const names = [];
  for (const k in cs) { if (isNaN(k) && typeof cs[k] === 'string' && k !== 'cssText') names.push(k); }
  names.sort();
  const out = names.map((k) => [k, cs[k]]);
  f.remove();
  return [['count', names.length, cs.length]].concat(out);
})()
