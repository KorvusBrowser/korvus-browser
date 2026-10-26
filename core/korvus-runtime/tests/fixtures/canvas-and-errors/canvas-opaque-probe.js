(() => {
  const r = [];
  const px = (x, X, Y) => Array.from(x.getImageData(X, Y, 1, 1).data);
  for (const attrs of [{alpha: false}, {alpha: false, willReadFrequently: true}]) {
    const c = new OffscreenCanvas(4, 4); const x = c.getContext('2d', attrs);
    const k = JSON.stringify(attrs);
    r.push([k + ' empty', px(x, 0, 0)]);
    x.fillStyle = 'rgba(255,0,0,0.5)'; x.fillRect(0, 0, 2, 2);
    r.push([k + ' half-red', px(x, 0, 0), px(x, 3, 3)]);
    x.clearRect(0, 0, 1, 1);
    r.push([k + ' cleared', px(x, 0, 0)]);
    x.globalCompositeOperation = 'copy'; x.fillStyle = 'rgba(0,0,255,0.25)'; x.fillRect(3, 3, 1, 1);
    r.push([k + ' copy-quarter-blue', px(x, 3, 3)]);
    r.push([k + ' attributes', JSON.stringify(x.getContextAttributes())]);
    c.width = 4;
    r.push([k + ' reset', px(x, 1, 1)]);
  }
  const h = document.createElement('canvas'); h.width = h.height = 2; const hx = h.getContext('2d', {alpha: false});
  hx.fillStyle = 'rgba(0,255,0,0.5)'; hx.fillRect(0, 0, 1, 1);
  r.push(['html canvas', px(hx, 0, 0), px(hx, 1, 1)]);
  return r;
})()
