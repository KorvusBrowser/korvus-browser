(async () => {
  // SVG text metrics as Blink's SvgTextQuery answers them: per family at 150px italic (the font probe's shape),
  // the same under a CSS scale(1.000998), a plain run, anchors, a tspan, collapsed whitespace, rotation,
  // hit testing, and a text that is not rendered.
  const R = [];
  const NS = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('width', '1200'); svg.setAttribute('height', '600');
  document.body.append(svg);
  const rect = (r) => r ? [r.x, r.y, r.width, r.height] : r;
  const pt = (p) => p ? [p.x, p.y] : p;
  const tryv = (f) => { try { return f(); } catch (e) { return 'threw ' + e.name + ': ' + e.message; } };
  const text = (attrs, content, style) => {
    const t = document.createElementNS(NS, 'text');
    for (const [k, v] of Object.entries(attrs)) t.setAttribute(k, v);
    if (style) t.style.cssText = style;
    if (typeof content === 'string') t.textContent = content; else for (const c of content) t.append(c);
    svg.append(t);
    return t;
  };
  const all = (t) => ({
    n: tryv(() => t.getNumberOfChars()),
    len: tryv(() => t.getComputedTextLength()),
    sub03: tryv(() => t.getSubStringLength(0, 3)),
    sub25: tryv(() => t.getSubStringLength(2, 5)),
    ext0: tryv(() => rect(t.getExtentOfChar(0))),
    extLast: tryv(() => rect(t.getExtentOfChar(t.getNumberOfChars() - 1))),
    start1: tryv(() => pt(t.getStartPositionOfChar(1))),
    end1: tryv(() => pt(t.getEndPositionOfChar(1))),
    rot0: tryv(() => t.getRotationOfChar(0)),
    bbox: tryv(() => rect(t.getBBox())),
  });
  for (const fam of ['serif', 'sans-serif', 'monospace', 'DejaVu Sans', 'Liberation Mono', 'Liberation Sans', 'NoSuchFont-xyz, serif']) {
    const t = text({ x: '10', y: '200' }, 'mmmmmmmmmmlli', 'font: italic 150px ' + fam);
    R.push(['italic 150px ' + fam, all(t)]);
    t.style.transform = 'scale(1.000998)';
    R.push(['scaled 1.000998 ' + fam, { len: tryv(() => t.getComputedTextLength()), sub03: tryv(() => t.getSubStringLength(0, 3)), ext0: tryv(() => rect(t.getExtentOfChar(0))), bbox: tryv(() => rect(t.getBBox())) }]);
    t.remove();
  }
  const plain = text({ x: '10', y: '20' }, 'Hello, world', 'font: 16px sans-serif');
  R.push(['plain 16px', all(plain)]);
  R.push(['hit (40, 15)', tryv(() => plain.getCharNumAtPosition({ x: 40, y: 15 })), 'hit (5, 15)', tryv(() => plain.getCharNumAtPosition({ x: 5, y: 15 })), 'hit (40, 99)', tryv(() => plain.getCharNumAtPosition({ x: 40, y: 99 }))]);
  plain.remove();
  for (const anchor of ['middle', 'end']) {
    const t = text({ x: '300', y: '50', 'text-anchor': anchor }, 'Anchored', 'font: 20px sans-serif');
    R.push(['anchor ' + anchor, { ext0: tryv(() => rect(t.getExtentOfChar(0))), start0: tryv(() => pt(t.getStartPositionOfChar(0))), bbox: tryv(() => rect(t.getBBox())) }]);
    t.remove();
  }
  const ts = document.createElementNS(NS, 'tspan'); ts.setAttribute('dx', '5'); ts.textContent = 'cd';
  const tt = text({ x: '10', y: '80' }, ['ab', ts, 'ef'], 'font: 20px sans-serif');
  R.push(['tspan: text', all(tt)]);
  R.push(['tspan: the tspan', { n: tryv(() => ts.getNumberOfChars()), len: tryv(() => ts.getComputedTextLength()), ext0: tryv(() => rect(ts.getExtentOfChar(0))), bbox: tryv(() => rect(ts.getBBox())) }]);
  tt.remove();
  const ws = text({ x: '10', y: '120' }, '  a   b  \n c ', 'font: 20px sans-serif');
  R.push(['whitespace', { n: tryv(() => ws.getNumberOfChars()), len: tryv(() => ws.getComputedTextLength()), start0: tryv(() => pt(ws.getStartPositionOfChar(0))) }]);
  ws.remove();
  const rt = text({ x: '10', y: '160', rotate: '30 45' }, 'abc', 'font: 20px sans-serif');
  R.push(['rotate', { rot0: tryv(() => rt.getRotationOfChar(0)), rot1: tryv(() => rt.getRotationOfChar(1)), rot2: tryv(() => rt.getRotationOfChar(2)) }]);
  rt.remove();
  const detached = document.createElementNS(NS, 'text'); detached.textContent = 'xyz';
  R.push(['detached', all(detached)]);
  const hidden = text({ x: '10', y: '20' }, 'xyz', 'display:none; font: 20px sans-serif');
  R.push(['display none', all(hidden)]);
  hidden.remove();
  const err = text({ x: '10', y: '20' }, 'xyz', 'font: 20px sans-serif');
  R.push(['errors', tryv(() => err.getSubStringLength(99, 1)), tryv(() => err.getExtentOfChar(3)), tryv(() => err.getStartPositionOfChar(-1)), tryv(() => err.selectSubString(5, 1)), tryv(() => err.getCharNumAtPosition({ x: NaN, y: 0 }))]);
  err.remove();
  svg.remove();
  return R;
})()
