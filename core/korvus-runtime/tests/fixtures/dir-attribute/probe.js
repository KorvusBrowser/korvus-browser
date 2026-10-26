(() => {
  const R = [];
  for (const t of ['div', 'canvas', 'span', 'p']) { const e = document.createElement(t); e.dir = 'rtl'; document.body.appendChild(e); const cs = getComputedStyle(e); R.push(t + ':' + cs.direction + '/' + cs.unicodeBidi); }
  for (const v of ['RTL', 'LtR', 'auto', 'bogus', '']) { const d = document.createElement('div'); d.setAttribute('dir', v); document.body.appendChild(d); const cs = getComputedStyle(d); R.push(JSON.stringify(v) + ':' + cs.direction + '/' + cs.unicodeBidi); }
  const p = document.createElement('div'); p.dir = 'rtl'; const ch = document.createElement('canvas'); p.appendChild(ch); document.body.appendChild(p); R.push(getComputedStyle(ch).direction, ch.getContext('2d').direction);
  p.dir = 'ltr'; R.push(getComputedStyle(ch).direction); p.removeAttribute('dir'); R.push(getComputedStyle(ch).direction);
  document.body.setAttribute('dir', 'bogus'); R.push('body bogus:' + getComputedStyle(document.body).direction); document.documentElement.dir = 'rtl'; R.push('body under rtl html:' + getComputedStyle(document.body).direction); document.body.removeAttribute('dir'); R.push('body plain under rtl html:' + getComputedStyle(document.body).direction);
  const s = document.createElement('div'); s.style.direction = 'ltr'; s.dir = 'rtl'; document.body.appendChild(s); R.push('author beats hint:' + getComputedStyle(s).direction);
  const ns = document.createElementNS('http://www.w3.org/2000/svg', 'g'); ns.setAttribute('dir', 'rtl'); document.body.appendChild(ns); R.push('svg:' + getComputedStyle(ns).direction);
  return R;
})()
