(async () => {
  // Computed fill / stroke for each paint form, set by the style attribute's CSSOM, by a presentation attribute, and
  // what the inline declaration reads back.
  const R = [];
  const NS = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(NS, 'svg'); document.body.append(svg);
  for (const v of ['none', 'red', 'url(#a)', 'url(#a) none', 'url(#a) red', 'context-fill', 'context-stroke', 'currentcolor', '#abc', 'transparent', 'rgba(1, 2, 3, 0.5)']) {
    const a = document.createElementNS(NS, 'path'); a.style.color = 'blue'; a.style.fill = v; a.style.stroke = v; svg.append(a);
    const b = document.createElementNS(NS, 'path'); b.setAttribute('fill', v); b.setAttribute('stroke', v); b.setAttribute('color', 'green'); svg.append(b);
    const sa = getComputedStyle(a), sb = getComputedStyle(b);
    R.push([v, a.style.fill, a.style.stroke, sa.fill, sa.stroke, sb.fill, sb.stroke]);
    a.remove(); b.remove();
  }
  svg.remove();
  return R;
})()
