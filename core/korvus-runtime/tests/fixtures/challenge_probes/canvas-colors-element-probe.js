(() => {
  const out = [];
  const sys = ['canvas', 'canvastext', 'highlight', 'highlighttext', 'buttonface', 'buttontext', 'buttonborder', 'field', 'fieldtext', 'graytext', 'linktext', 'visitedtext', 'activetext', 'mark', 'marktext', 'selecteditem', 'selecteditemtext', 'activeborder', 'activecaption', 'appworkspace', 'background', 'buttonhighlight', 'buttonshadow', 'captiontext', 'inactiveborder', 'inactivecaption', 'inactivecaptiontext', 'infobackground', 'infotext', 'menu', 'menutext', 'scrollbar', 'threeddarkshadow', 'threedface', 'threedhighlight', 'threedlightshadow', 'threedshadow', 'window', 'windowframe', 'windowtext'];
  const el = document.createElement('canvas'); document.body.appendChild(el); const x = el.getContext('2d');
  const oc = new OffscreenCanvas(1, 1).getContext('2d');
  for (const s of sys) { x.fillStyle = '#123456'; x.fillStyle = s; oc.fillStyle = '#123456'; oc.fillStyle = s; out.push([s, x.fillStyle, oc.fillStyle]); }
  // currentColor resolution
  const cases = [['inline red', c => { c.style.color = 'red'; }], ['inline rgb()', c => { c.style.color = 'rgb(10 20 30 / 50%)'; }], ['inline lab', c => { c.style.color = 'lab(50 40 30)'; }],
                 ['inline currentcolor', c => { c.style.color = 'currentcolor'; }], ['inline highlight', c => { c.style.color = 'highlight'; }], ['class only', c => { c.className = 'k'; }],
                 ['inherited', c => { document.body.style.color = 'blue'; }], ['no style', c => {}]];
  const st = document.createElement('style'); st.textContent = '.k { color: green }'; document.head.appendChild(st);
  for (const [name, f] of cases) {
    const c = document.createElement('canvas'); document.body.appendChild(c); f(c); const y = c.getContext('2d');
    y.fillStyle = 'currentcolor'; const a = y.fillStyle; y.fillStyle = '#123456';
    c.remove(); y.fillStyle = 'currentcolor'; const b = y.fillStyle;
    y.shadowColor = 'currentcolor'; const sh = y.shadowColor;
    out.push(['currentColor ' + name, a, b, sh]);
    document.body.style.color = '';
  }
  // non-string fillStyle values
  const z = document.createElement('canvas').getContext('2d');
  for (const v of [0, 123, null, undefined, true, {toString() { return 'red'; }}, ['blue'], {}]) {
    z.fillStyle = '#123456';
    try { z.fillStyle = v; out.push(['value ' + String(v), z.fillStyle]); } catch (e) { out.push(['value ' + String(v), 'THROW ' + e.name]); }
  }
  try { z.fillStyle = { toString() { throw new TypeError('boom'); } }; out.push(['throwing toString', z.fillStyle]); } catch (e) { out.push(['throwing toString', 'THROW ' + e.name + ' ' + e.message]); }
  // shadowColor + addColorStop parsing
  for (const s of ['color(display-p3 1 0 0)', 'currentcolor', 'violetred', 'bogus', ' red ', 'rgba(1,2,3,0.123)']) {
    z.shadowColor = '#123456'; z.shadowColor = s;
    const g = z.createLinearGradient(0, 0, 1, 0); let r;
    try { g.addColorStop(0.5, s); r = 'ok'; } catch (e) { r = e.name + ': ' + e.message; }
    out.push(['shadow/stop ' + s, z.shadowColor, r]);
  }
  return out;
})()
