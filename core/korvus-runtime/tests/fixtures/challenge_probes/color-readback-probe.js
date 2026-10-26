(() => {
  const out = [];
  for (const ctxCs of ['srgb', 'display-p3']) for (const readCs of [undefined, 'srgb', 'display-p3']) for (const fmt of [undefined, 'rgba-unorm8', 'rgba-float16']) {
    try {
      const c = new OffscreenCanvas(2, 2); const x = c.getContext('2d', { colorSpace: ctxCs });
      x.fillStyle = 'color(display-p3 1 0.25 0.5)'; x.fillRect(0, 0, 2, 2);
      const s = {}; if (readCs) s.colorSpace = readCs; if (fmt) s.pixelFormat = fmt;
      const d = x.getImageData(0, 0, 1, 1, s);
      out.push([ctxCs + '/' + readCs + '/' + fmt, d.colorSpace, d.pixelFormat, Array.from(d.data), x.fillStyle, JSON.stringify(x.getContextAttributes())]);
    } catch (e) { out.push([ctxCs + '/' + readCs + '/' + fmt, 'THROW ' + e.name + ': ' + e.message]); }
  }
  for (const col of ['#ff8000', 'rgb(10, 200, 30)', 'color(srgb 0.2 0.4 0.6)', 'color(display-p3 0 1 0)', 'color(display-p3 1 0.25 0.5 / 0.5)', 'lab(50 40 30)', 'oklch(0.7 0.2 150)']) {
    for (const ctxCs of ['srgb', 'display-p3']) {
      const c = new OffscreenCanvas(1, 1); const x = c.getContext('2d', { colorSpace: ctxCs });
      x.fillStyle = col; x.fillRect(0, 0, 1, 1);
      out.push([col + ' in ' + ctxCs, x.fillStyle, Array.from(x.getImageData(0, 0, 1, 1).data), Array.from(x.getImageData(0, 0, 1, 1, { colorSpace: ctxCs === 'srgb' ? 'display-p3' : 'srgb' }).data)]);
    }
  }
  return out;
})()
