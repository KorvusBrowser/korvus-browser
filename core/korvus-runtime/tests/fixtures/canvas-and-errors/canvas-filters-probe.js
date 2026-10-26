(() => {
  const H = (d) => { let h = 2166136261; for (let i = 0; i < d.length; i++) { h ^= d[i]; h = Math.imul(h, 16777619); } return (h >>> 0).toString(16); };
  const r = [];
  const src = new OffscreenCanvas(32, 8); const s = src.getContext('2d', {willReadFrequently: true});
  for (let i = 0; i < 32; i++) for (let j = 0; j < 8; j++) { s.fillStyle = `rgba(${(i * 37 + j * 11) % 256},${(i * 13 + j * 71) % 256},${(i * 89 + j * 5) % 256},${((i * 7 + j * 29) % 256) / 255})`; s.fillRect(i, j, 1, 1); }
  for (const f of ['grayscale(1)', 'grayscale(0.3)', 'sepia(0.6)', 'saturate(2.5)', 'hue-rotate(77deg)', 'invert(0.35)', 'opacity(0.45)', 'brightness(1.6)', 'contrast(0.4)', 'grayscale(1) invert(0.5)']) {
    for (const [name, attrs] of [['alpha', {willReadFrequently: true}], ['opaque', {alpha: false, willReadFrequently: true}]]) {
      const c = new OffscreenCanvas(32, 8); const x = c.getContext('2d', attrs); x.filter = f; x.drawImage(src, 0, 0);
      r.push([f + ' ' + name, H(x.getImageData(0, 0, 32, 8).data)]);
    }
  }
  return r;
})()
