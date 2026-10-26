(async () => {
  // SVG presentation attributes (Blink's SVGElement::CollectStyleForPresentationAttribute): an attribute named as a
  // CSS property on an SVG element is a declaration below every author rule, parsed in SVG attribute mode (a
  // unitless length is px); an invalid value is dropped whole. Each row: the attributes, then what getComputedStyle
  // and the text metrics read.
  const R = [];
  const NS = 'http://www.w3.org/2000/svg';
  const st = document.createElement('style');
  st.textContent = '.big { font-size: 40px } .blue { fill: blue }';
  document.head.append(st);
  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('width', '800'); svg.setAttribute('height', '400');
  document.body.append(svg);
  const cs = (el, props) => { const s = getComputedStyle(el); return props.map((p) => p + '=' + s.getPropertyValue(p)); };
  const mk = (tag, attrs, parent = svg, content) => {
    const e = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
    if (content !== undefined) e.textContent = content;
    parent.append(e);
    return e;
  };
  const tl = (t) => { try { return t.getComputedTextLength(); } catch (e) { return 'threw ' + e.name; } };
  let t = mk('text', { x: '10', y: '50', 'font-size': '20', 'font-family': 'DejaVu Sans', 'text-anchor': 'middle', fill: 'red', 'stroke-width': '2', 'letter-spacing': '1', 'font-weight': '700', 'font-style': 'italic' }, svg, 'mmmmmmmmmmlli');
  R.push(['text: font attrs', cs(t, ['font-size', 'font-family', 'text-anchor', 'fill', 'stroke-width', 'letter-spacing', 'font-weight', 'font-style']), tl(t)]);
  t.remove();
  t = mk('text', { x: '10', y: '50', 'font-size': '20', 'font-family': 'serif' }, svg, 'mmmmmmmmmmlli');
  R.push(['text: 20 serif', cs(t, ['font-size', 'font-family']), tl(t), t.getBBox().height]);
  t.remove();
  const g = mk('g', { 'font-size': '30', fill: 'blue', 'font-family': 'Liberation Mono' });
  t = mk('text', { x: '10', y: '50' }, g, 'inherit');
  R.push(['g attrs inherited', cs(t, ['font-size', 'fill', 'font-family']), tl(t)]);
  g.remove();
  t = mk('text', { x: '10', y: '50', 'font-size': '20', style: 'font-size: 30px' }, svg, 'abc');
  R.push(['inline style beats attr', cs(t, ['font-size'])]);
  t.remove();
  t = mk('text', { x: '10', y: '50', 'font-size': '20', class: 'big', fill: 'red' }, svg, 'abc');
  t.classList.add('blue');
  R.push(['sheet beats attr', cs(t, ['font-size', 'fill'])]);
  t.remove();
  t = mk('text', { x: '10', y: '50', fill: 'red; display:none', 'font-size': '20 !important', stroke: 'nonsense' }, svg, 'abc');
  R.push(['invalid values dropped whole', cs(t, ['fill', 'display', 'font-size', 'stroke'])]);
  t.remove();
  t = mk('text', { x: '10', y: '50', 'font-size': 'abc', 'stroke-width': '-1', opacity: '0.5', visibility: 'hidden' }, svg, 'abc');
  R.push(['bad numbers, opacity, visibility', cs(t, ['font-size', 'stroke-width', 'opacity', 'visibility'])]);
  t.remove();
  for (const v of ['none', 'url(#a)', 'url(#a) red', 'context-stroke', 'currentcolor']) {
    const q = mk('path', { d: 'M0 0 L10 10', fill: v, stroke: v, color: 'green' });
    R.push(['paint attribute ' + v, cs(q, ['fill', 'stroke'])]);
    q.remove();
  }
  const p = mk('path', { d: 'M0 0 L10 10', 'stroke-dasharray': '5 10', 'stroke-dashoffset': '3', 'stroke-linecap': 'round', 'stroke-miterlimit': '7', 'fill-rule': 'evenodd', 'stroke-opacity': '0.25' });
  R.push(['path stroke attrs', cs(p, ['stroke-dasharray', 'stroke-dashoffset', 'stroke-linecap', 'stroke-miterlimit', 'fill-rule', 'stroke-opacity'])]);
  p.remove();
  t = mk('text', { x: '10', y: '50', 'baseline-shift': '2', 'word-spacing': '4', 'transform-origin': '10 20', 'letter-spacing': '0', 'dominant-baseline': 'middle', 'writing-mode': 'vertical-rl' }, svg, 'abc');
  R.push(['lengths unitless', cs(t, ['baseline-shift', 'word-spacing', 'transform-origin', 'letter-spacing', 'dominant-baseline', 'writing-mode'])]);
  t.remove();
  const div = document.createElement('div'); div.setAttribute('fill', 'red'); div.setAttribute('font-size', '30'); document.body.append(div);
  R.push(['html element ignores them', cs(div, ['fill', 'font-size'])]);
  div.remove();
  t = mk('text', { x: '10', y: '50', FILL: 'red', 'Font-Size': '30' }, svg, 'abc');
  R.push(['names are case-sensitive', cs(t, ['fill', 'font-size'])]);
  t.remove();
  t = mk('text', { x: '10', y: '50', display: 'none', 'font-size': '20' }, svg, 'abc');
  R.push(['display none', cs(t, ['display']), tl(t), t.getNumberOfChars()]);
  t.remove();
  t = mk('text', { x: '10', y: '50', 'font-family': "'Liberation Mono', monospace", color: 'red', fill: 'currentColor' }, svg, 'abc');
  R.push(['quoted family, currentColor', cs(t, ['font-family', 'fill', 'color']), tl(t)]);
  t.remove();
  t = mk('text', { x: '10', y: '50', 'xml:space': 'preserve' }, svg, '  a  b  ');
  t.setAttributeNS('http://www.w3.org/XML/1998/namespace', 'xml:space', 'preserve');
  R.push(['xml:space preserve', cs(t, ['white-space-collapse']), t.getNumberOfChars()]);
  t.remove();
  t = mk('text', { x: '10', y: '50', 'font-size': '20' }, svg, 'abc');
  const before = tl(t);
  t.setAttribute('font-size', '40');
  R.push(['attr change restyles', before, tl(t), cs(t, ['font-size'])]);
  t.removeAttribute('font-size');
  R.push(['attr removed', tl(t), cs(t, ['font-size'])]);
  t.remove();
  svg.remove();
  return R;
})()
