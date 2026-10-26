(() => {
  const out = [];
  const el = document.createElement('div'); el.style.color = 'red'; document.body.appendChild(el);
  const cs = getComputedStyle(el), st = el.style;
  for (const [nm, o] of [['computed', cs], ['inline', st]]) {
    const own = Object.getOwnPropertyNames(o), keys = Object.keys(o);
    const named = own.filter(k => !/^\d+$/.test(k));
    out.push([nm + ' counts', own.length, keys.length, named.length, o.length]);
    out.push([nm + ' first named', named.slice(0, 12).join(',')]);
    out.push([nm + ' last named', named.slice(-12).join(',')]);
    out.push([nm + ' own-not-keys', own.filter(k => !keys.includes(k)).join(',')]);
    out.push([nm + ' order idx-first', own.slice(0, 3).join(','), own.slice(o.length - 1, o.length + 2).join(',')]);
    let forin = []; for (const k in o) { forin.push(k); if (forin.length > 2000) break; }
    out.push([nm + ' for-in count', forin.length, forin.slice(0, 3).join(','), forin.slice(-3).join(',')]);
    out.push([nm + ' in', 'accentColor' in o, 'background-color' in o, 'webkitAppearance' in o, 'WebkitAppearance' in o, 'foo' in o, 'cssFloat' in o, 'float' in o, '--x' in o, 'borderRightColor' in o, 'border-rightColor' in o]);
    const d = (k) => { const x = Object.getOwnPropertyDescriptor(o, k); return x ? JSON.stringify(x) : 'none'; };
    out.push([nm + ' desc', d('accentColor'), d('background-color'), d('cssFloat'), d('float'), d('foo'), d('WebkitAppearance'), d('webkitAppearance')]);
    out.push([nm + ' get', o['background-color'], o.backgroundColor, o.WebkitAppearance, o.webkitAppearance, String(o.foo), o.float, o.cssFloat]);
  }
  try { cs.color = 'blue'; out.push(['computed set', cs.color]); } catch (e) { out.push(['computed set THROW', e.name, e.message]); }
  try { cs['background-color'] = 'blue'; out.push(['computed set dashed', 'ok']); } catch (e) { out.push(['computed set dashed THROW', e.name, e.message]); }
  try { cs.foo = 1; out.push(['computed set foo', cs.foo, Object.getOwnPropertyNames(cs).includes('foo')]); } catch (e) { out.push(['computed set foo THROW', e.name]); }
  st.backgroundColor = 'green'; out.push(['inline set camel', st.backgroundColor, st.cssText]);
  st['border-top-color'] = 'blue'; out.push(['inline set dashed', st.borderTopColor, st.cssText]);
  st.marginTop = 5; out.push(['inline set number', st.marginTop]);
  st.width = 0; out.push(['inline set zero', st.width, st.cssText]);
  out.push(['delete', delete st.color, st.color, delete st.foo]);
  st.foo = 'bar'; out.push(['expando', st.foo, Object.getOwnPropertyNames(st).includes('foo'), Object.keys(st).includes('foo')]);
  out.push(['proto has', Object.getOwnPropertyNames(CSSStyleDeclaration.prototype).length, 'accentColor' in CSSStyleDeclaration.prototype]);
  return out;
})()
