(() => {
  const out = [];
  const el = document.createElement('div'); el.style.color = 'red'; el.style.marginTop = '2px'; document.body.appendChild(el);
  const cs = getComputedStyle(el), st = el.style;
  const D = (o, k) => { const x = Object.getOwnPropertyDescriptor(o, k); return x ? JSON.stringify(x) : 'none'; };
  out.push(['idx desc', D(cs, '0'), D(st, '0'), D(st, '1'), D(st, '2'), D(cs, '474'), D(cs, '473')]);
  out.push(['idx in', 0 in st, 1 in st, 2 in st, 473 in cs, 474 in cs]);
  try { 'use strict'; cs[0] = 'x'; out.push(['idx set', cs[0]]); } catch (e) { out.push(['idx set THROW', e.name, e.message]); }
  out.push(['idx set sloppy', (function () { st[0] = 'x'; return st[0]; })()]);
  out.push(['delete idx', delete st[0], st[0], delete cs[0]]);
  out.push(['named list sample', Object.getOwnPropertyNames(document.createElement('p').style).length]);
  const names = Object.getOwnPropertyNames(document.createElement('p').style);
  out.push(['has float', names.includes('float'), names.includes('cssFloat'), names.includes('webkitAppearance'), names.includes('WebkitAppearance'), names.filter(n => /^webkit/.test(n)).length, names.filter(n => /^epub/.test(n)).length]);
  out.push(['set computed strict', (() => { try { (function () { 'use strict'; cs.color = 'x'; })(); return 'no throw'; } catch (e) { return e.name + ': ' + e.message; } })()]);
  out.push(['set computed invalid', (() => { try { cs.fooBar = 1; return [cs.fooBar, Object.prototype.hasOwnProperty.call(cs, 'fooBar')]; } catch (e) { return 'THROW ' + e.name; } })()]);
  out.push(['set computed webkit', (() => { try { cs.webkitAppearance = 'none'; return 'ok'; } catch (e) { return e.name + ': ' + e.message; } })()]);
  out.push(['rule style', (() => { const s = document.createElement('style'); s.textContent = 'a{color:red}'; document.head.appendChild(s); const r = s.sheet.cssRules[0].style; return [Object.getOwnPropertyNames(r).length, Object.keys(r).length, r[0], 'color' in r, D(r, '0'), D(r, 'color')]; })()]);
  return out;
})()
