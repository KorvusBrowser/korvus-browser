(async () => {
  const R = [];
  const T = (o) => Object.prototype.toString.call(o);
  const run = (path, opts = {}) => new Promise((resolve) => {
    const x = new XMLHttpRequest();
    const ev = [];
    const rec = (e) => {
      let tl; try { tl = x.responseText.length; } catch (err) { tl = err.name; }
      const row = [e.type, x.readyState];
      if (e.type !== 'readystatechange') row.push(e.loaded, e.total, e.lengthComputable);
      row.push(tl, x.status, x.responseURL.replace(location.origin, ''));
      ev.push(row);
      if (opts.onEvent) opts.onEvent(x, e);
    };
    for (const ty of ['readystatechange', 'loadstart', 'progress', 'load', 'loadend', 'abort', 'error', 'timeout']) x.addEventListener(ty, rec);
    x.open('GET', path);
    x.send();
    for (const p of opts.peeks || []) setTimeout(() => ev.push(['peek', p, x.readyState, x.status, x.responseURL.replace(location.origin, ''), x.getResponseHeader('content-type')]), p);
    setTimeout(() => { if (opts.after) opts.after(x, ev); resolve(ev); }, opts.keep || 1000);
  });
  const H = '/p?ct=text/html;charset=utf-8&d=';
  R.push(['peeks, text/html', await run(H + '300:100,300:100', {peeks: [150, 450], keep: 900})]);
  R.push(['peeks, text/plain held', await run('/p?ct=text/plain&d=300:100,300:1000', {peeks: [150, 450], keep: 900})]);
  R.push(['open in readystatechange 2', await run(H + '300:1000,300:1000', {keep: 1000, onEvent: (x, e) => { if (e.type === 'readystatechange' && x.readyState === 2 && !x.__d) { x.__d = 1; x.open('GET', '/p?d=0:1'); } },
    after: (x, ev) => { let s; try { x.send(); s = 'sent'; } catch (err) { s = err.name; } ev.push(['send after', s, x.readyState]); }})]);
  R.push(['abort in readystatechange 2, empty body', await run(H, {keep: 600, onEvent: (x, e) => { if (e.type === 'readystatechange' && x.readyState === 2 && !x.__d) { x.__d = 1; x.abort(); } }})]);
  R.push(['open in readystatechange 2, empty body', await run(H, {keep: 600, onEvent: (x, e) => { if (e.type === 'readystatechange' && x.readyState === 2 && !x.__d) { x.__d = 1; x.open('GET', '/p?d=0:1'); } }})]);
  R.push(['abort in a later progress', await run(H + '300:100,300:100,300:100', {keep: 1300, onEvent: (x, e) => { if (e.type === 'progress' && e.loaded === 200 && !x.__d) { x.__d = 1; x.abort(); } }})]);
  R.push(['open+send in readystatechange 3', await run(H + '300:1000,300:1000', {keep: 1300, onEvent: (x, e) => { if (e.type === 'readystatechange' && x.readyState === 3 && !x.__d) { x.__d = 1; x.open('GET', H + '0:7'); x.send(); } }})]);
  return R;
})()
