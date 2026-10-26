(async () => {
  // How a transform's numbers keep their precision: the inline declaration as written back, the computed matrix.
  const R = [];
  const NS = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(NS, 'svg'); document.body.append(svg);
  const t = document.createElementNS(NS, 'text'); svg.append(t);
  const d = document.createElement('div'); document.body.append(d);
  for (const v of ['scale(1.000998)', 'scale(1.0009983)', 'scale(1.00099834567)', 'rotate(0.1234567deg)', 'translate(1.23456789px, 2px)', 'matrix(1.0000001, 0, 0, 1, 0, 0)', 'scale(123456.789)', 'scale(0.000012345)']) {
    t.style.transform = v; d.style.transform = v;
    R.push([v, t.style.transform, getComputedStyle(t).transform, d.style.transform, getComputedStyle(d).transform, d.style.cssText]);
  }
  for (const v of ['1.000998', '0.1234567', '1234567.5', '0.0000001']) {
    d.style.opacity = v; d.style.width = v + 'px'; d.style.lineHeight = v;
    R.push(['num ' + v, d.style.opacity, d.style.width, d.style.lineHeight, getComputedStyle(d).opacity, getComputedStyle(d).width, getComputedStyle(d).lineHeight]);
  }
  svg.remove(); d.remove();
  return R;
})()
