(async () => {
  // The ink box of italic text: canvas measureText's actual bounding box vs an SVG <text>'s getBBox, per family.
  const R = [];
  const NS = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(NS, 'svg'); svg.setAttribute('width', '1600'); svg.setAttribute('height', '400');
  document.body.append(svg);
  const ctx = document.createElement('canvas').getContext('2d');
  for (const fam of ['serif', 'sans-serif', 'monospace', 'DejaVu Sans', 'Liberation Sans', 'Liberation Mono', 'Noto Serif CJK KR']) {
    for (const style of ['italic', 'normal', 'oblique']) {
      for (const s of ['i', 'mmmmmmmmmmlli', 'l']) {
        ctx.font = style + ' 150px ' + fam;
        const m = ctx.measureText(s);
        const t = document.createElementNS(NS, 'text'); t.setAttribute('x', '10'); t.setAttribute('y', '200');
        t.style.font = style + ' 150px ' + fam; t.textContent = s; svg.append(t);
        const b = t.getBBox();
        R.push([fam + ' ' + style + ' ' + s, 'canvas', [m.width, m.actualBoundingBoxLeft, m.actualBoundingBoxRight, m.actualBoundingBoxAscent, m.actualBoundingBoxDescent], 'svg', [t.getComputedTextLength(), b.x, b.y, b.width, b.height]]);
        t.remove();
      }
    }
  }
  svg.remove();
  return R;
})()
