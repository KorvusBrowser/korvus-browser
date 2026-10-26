(async () => {
  const out = [];
  const f = document.createElement('iframe');
  f.srcdoc = '<p>x';
  let n = 0;
  const seen = [];
  f.onload = () => {
    n++;
    seen.push('load' + n + ':' + f.contentDocument.readyState);
    if (n === 1) {
      const d = f.contentDocument;
      d.write('<!doctype html><p>written');
      d.close();
      seen.push('closed:' + d.readyState + ':' + n + ':' + d.body.textContent);
    }
  };
  document.body.appendChild(f);
  await new Promise((r) => setTimeout(r, 300));
  out.push(['inside-load', n, seen.join(',')]);
  const before = n;
  const d = f.contentDocument;
  d.open();
  d.write('<!doctype html><p>again');
  d.close();
  out.push(['outside-load', n - before, d.readyState, d.body.textContent]);
  await new Promise((r) => setTimeout(r, 300));
  out.push(['outside-load-later', n - before, seen.slice(2).join(',')]);
  return out;
})()
