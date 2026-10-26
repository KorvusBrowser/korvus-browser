(() => {
  const H = (d) => { let h = 2166136261; for (let i = 0; i < d.length; i++) { h ^= d[i]; h = Math.imul(h, 16777619); } return (h >>> 0).toString(16); };
  const r = [];
  const C = (w, h, attrs) => { const c = new OffscreenCanvas(w, h); return [c, c.getContext('2d', attrs || {willReadFrequently: true})]; };
  const P = (n, x, w, h) => r.push([n, H(x.getImageData(0, 0, w, h).data)]);
  // an 8x1 grey strip drawn narrower, at every width and at each smoothing quality
  const [strip, sx] = C(8, 1);
  [0, 255, 0, 255, 40, 200, 90, 160].forEach((v, i) => { sx.fillStyle = `rgb(${v},${v},${v})`; sx.fillRect(i, 0, 1, 1); });
  for (const w of [7, 6, 5, 4, 3, 2, 1]) { const [, x] = C(8, 1); x.drawImage(strip, 0, 0, 8, 1, 0, 0, w, 1); P('strip ' + w, x, 8, 1); }
  for (const q of ['low', 'medium', 'high']) { const [, x] = C(8, 1); x.imageSmoothingQuality = q; x.drawImage(strip, 0, 0, 8, 1, 0, 0, 3, 1); P('strip 3 ' + q, x, 8, 1); }
  // a 9x7 translucent source scaled down, up, unevenly, through the transform, from a sub-rectangle and as a pattern
  const [src, s] = C(9, 7);
  for (let y = 0; y < 7; y++) for (let x = 0; x < 9; x++) {
    const a = ((x + y) % 3 === 0) ? 1 : 0.4 + 0.1 * ((x * y) % 6);
    s.fillStyle = `rgba(${(x * 53 + y * 17) % 256},${(x * 29 + y * 71) % 256},${(x * 101 + y * 37) % 256},${a})`; s.fillRect(x, y, 1, 1);
  }
  const D = (name, q, w, h, f) => { const [, x] = C(w, h); if (q === 'off') x.imageSmoothingEnabled = false; else x.imageSmoothingQuality = q; f(x); P(name + ' ' + q, x, w, h); };
  for (const q of ['low', 'medium', 'high', 'off']) {
    D('down 4x3', q, 4, 3, (x) => x.drawImage(src, 0, 0, 4, 3));
    D('down 2x2', q, 2, 2, (x) => x.drawImage(src, 0, 0, 2, 2));
    D('up 23x17', q, 23, 17, (x) => x.drawImage(src, 0, 0, 23, 17));
    D('mixed 20x3', q, 20, 3, (x) => x.drawImage(src, 0, 0, 20, 3));
    D('ctm up', q, 20, 16, (x) => { x.scale(2.5, 2.5); x.drawImage(src, 0, 0, 7, 6); });
    D('ctm down', q, 6, 5, (x) => { x.scale(0.5, 0.5); x.drawImage(src, 0, 0, 11, 9); });
    D('sub-rect up', q, 15, 12, (x) => x.drawImage(src, 2, 1, 5, 4, 0, 0, 15, 12));
    D('pattern down', q, 6, 5, (x) => { const p = x.createPattern(src, 'repeat'); p.setTransform(new DOMMatrix().scale(0.4, 0.45)); x.fillStyle = p; x.fillRect(0, 0, 6, 5); });
    D('pattern up', q, 18, 14, (x) => { const p = x.createPattern(src, 'repeat'); p.setTransform(new DOMMatrix().scale(1.7, 1.9)); x.fillStyle = p; x.fillRect(0, 0, 18, 14); });
  }
  // the challenge's own: its 80x24 text strip read back at 1x1 and 4x4 on alpha and opaque canvases, its arcs
  for (const [an, attrs] of [['alpha', {willReadFrequently: true}], ['opaque', {alpha: false, willReadFrequently: true}]]) {
    const [a, ax] = C(80, 24, attrs);
    ax.fillStyle = '#ffffff'; ax.fillRect(0, 0, 80, 24); ax.fillStyle = '#000000'; ax.font = '16px Arial'; ax.textBaseline = 'top'; ax.fillText('Hwmqxli', 2, 4);
    const [, d1] = C(1, 1, attrs); d1.drawImage(a, 0, 0, 80, 24, 0, 0, 1, 1); P('text to 1x1 ' + an, d1, 1, 1);
    const [, d4] = C(4, 4, attrs); d4.drawImage(a, 0, 0, 80, 24, 0, 0, 4, 4); P('text to 4x4 ' + an, d4, 4, 4);
  }
  {
    const [, x] = C(49, 44);
    const disc = (cx, cy, rad) => { x.beginPath(); x.arc(cx, cy, rad, 0, Math.PI * 2, true); x.closePath(); x.fill(); };
    x.scale(0.4, 0.4); x.fillStyle = '#f2f'; disc(40, 40, 40);
    x.globalCompositeOperation = 'multiply'; x.fillStyle = '#2ff'; disc(80, 40, 40); x.fillStyle = '#ff2'; disc(60, 80, 40);
    x.globalCompositeOperation = 'source-over'; x.fillStyle = '#fff';
    x.beginPath(); x.arc(61, 53, 20, 0, Math.PI * 2, true); x.arc(61, 53, 10, 0, Math.PI * 2, true); x.fill('evenodd');
    P('arcs 49x44', x, 49, 44);
  }
  { const [, x] = C(16, 16); x.fillStyle = '#0000ff'; x.beginPath(); x.arc(8, 8, 5, 0, Math.PI * 2); x.fill(); P('arc 16x16', x, 16, 16); }
  // a destination under one device pixel wide
  { const [s4, s4x] = C(4, 4); s4x.fillStyle = '#c06'; s4x.fillRect(0, 0, 4, 4); const [, x] = C(8, 8); x.drawImage(s4, 1.3, 1.3, 0.6, 3); P('sub-pixel destination', x, 8, 8); }
  return r;
})()
