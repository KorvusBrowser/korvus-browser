(() => {
  const H = (d) => { let h = 2166136261; for (let i = 0; i < d.length; i++) { h ^= d[i]; h = Math.imul(h, 16777619); } return (h >>> 0).toString(16); };
  const r = [];
  const src = new OffscreenCanvas(4, 4); const s = src.getContext('2d', {willReadFrequently: true}); s.fillStyle = '#c06'; s.fillRect(0, 0, 4, 4);
  const P = (n, f) => { const c = new OffscreenCanvas(8, 8); const x = c.getContext('2d', {willReadFrequently: true}); f(x); const d = x.getImageData(0, 0, 8, 8).data; r.push([n, H(d), Array.from({length: 8}, (_, i) => d[(8 + i) * 4 + 3])]); };
  P('1:1 at 0.5', (x) => x.drawImage(src, 0.5, 0.5));
  P('1:1 at 0.25,0.75', (x) => x.drawImage(src, 0.25, 0.75));
  P('2x at 0.5', (x) => x.drawImage(src, 0.5, 0.5, 8, 8));
  P('rotated', (x) => { x.rotate(0.2); x.drawImage(src, 2, 1); });
  return r;
})()
