(() => {
  const R = [];
  const px = (x, a, b) => Array.from(x.getImageData(a, b, 1, 1).data).join(',');
  const oc = new OffscreenCanvas(400, 200); const x = oc.getContext('2d');
  R.push(['own names', JSON.stringify(Object.getOwnPropertyNames(x))]);
  R.push(['own symbols', String(Object.getOwnPropertySymbols(x).length)]);
  R.push(['canvas is oc', String(x.canvas === oc)]);
  R.push(['hasOwn canvas', String(Object.prototype.hasOwnProperty.call(x, 'canvas'))]);
  const d = Object.getOwnPropertyDescriptor(OffscreenCanvasRenderingContext2D.prototype, 'canvas');
  R.push(['proto canvas', d ? [typeof d.get, typeof d.set, d.enumerable, d.configurable, d.get && d.get.name, d.get && d.get.length].join(',') : 'none']);
  try { d.get.call({}); R.push(['getter on {}', 'no throw']); } catch (e) { R.push(['getter on {}', e.name + ': ' + e.message]); }
  try { R.push(['oc getter on el ctx', String(d.get.call(document.createElement('canvas').getContext('2d')))]); } catch (e) { R.push(['oc getter on el ctx', e.name + ': ' + e.message]); }
  const ed = Object.getOwnPropertyDescriptor(CanvasRenderingContext2D.prototype, 'canvas');
  try { R.push(['el getter on oc ctx', String(ed.get.call(x))]); } catch (e) { R.push(['el getter on oc ctx', e.name + ': ' + e.message]); }
  x.fillStyle = '#123456'; x.fillRect(350, 180, 20, 10);
  R.push(['read 360,185', px(x, 360, 185)]);
  R.push(['read 0,0', px(x, 0, 0)]);
  const big = x.getImageData(0, 0, 400, 200); let n = 0; for (let i = 3; i < big.data.length; i += 4) if (big.data[i]) n++;
  R.push(['nonzero alpha', String(n)]);
  R.push(['read beyond', px(x, 399, 199) + '|' + px(x, 405, 5)]);
  const ex = document.createElement('canvas').getContext('2d');
  oc.width = 10; R.push(['after resize', [x.getImageData(0, 0, 10, 10).width, px(x, 0, 0), oc.width, oc.height].join('|')]);
  const e2 = document.createElement('canvas'); e2.width = 500; e2.height = 300;
  const t = e2.transferControlToOffscreen(); const tx = t.getContext('2d'); tx.fillStyle = '#abcdef'; tx.fillRect(450, 250, 10, 10);
  R.push(['transferred', px(tx, 455, 255) + '|' + String(tx.canvas === t) + '|' + JSON.stringify(Object.getOwnPropertyNames(tx))]);
  R.push(['el own names', JSON.stringify(Object.getOwnPropertyNames(ex))]);
  for (const k of ['webgl', 'webgl2', 'bitmaprenderer']) {
    try { const o = new OffscreenCanvas(5, 5).getContext(k); R.push([k + ' own', o ? JSON.stringify(Object.getOwnPropertyNames(o)) : 'null']); } catch (err) { R.push([k + ' own', err.name]); }
  }
  for (const id of ['2D', ' 2d', 'WebGL', 'BitmapRenderer']) {
    try { const o = new OffscreenCanvas(5, 5).getContext(id); R.push(['getContext ' + JSON.stringify(id), o === null ? 'null' : Object.prototype.toString.call(o)]); } catch (err) { R.push(['getContext ' + JSON.stringify(id), err.name]); }
    try { const o = document.createElement('canvas').getContext(id); R.push(['el getContext ' + JSON.stringify(id), o === null ? 'null' : Object.prototype.toString.call(o)]); } catch (err) { R.push(['el getContext ' + JSON.stringify(id), err.name]); }
  }
  // direction snapshots
  const d0 = new OffscreenCanvas(5, 5).getContext('2d'); d0.direction = 'inherit';
  document.documentElement.dir = 'rtl';
  const d1 = new OffscreenCanvas(5, 5).getContext('2d'); d1.direction = 'inherit';
  R.push(['dir before/after', d0.direction + '|' + d1.direction]);
  document.documentElement.dir = 'RTL';
  R.push(['dir RTL', new OffscreenCanvas(5, 5).getContext('2d').direction]);
  document.documentElement.removeAttribute('dir');
  R.push(['dir removed', new OffscreenCanvas(5, 5).getContext('2d').direction + '|' + d1.direction]);
  const e3 = document.createElement('canvas'); e3.style.direction = 'rtl'; document.body.appendChild(e3);
  const t3 = e3.transferControlToOffscreen(); R.push(['transfer rtl', t3.getContext('2d').direction]);
  const e4 = document.createElement('canvas'); e4.dir = 'rtl';
  const t4 = e4.transferControlToOffscreen(); R.push(['transfer detached dir=rtl', t4.getContext('2d').direction]);
  const e5 = document.createElement('canvas'); e5.dir = 'rtl'; document.body.appendChild(e5);
  R.push(['el dir=rtl ctx', e5.getContext('2d').direction]);
  const e6 = document.createElement('canvas'); e6.dir = 'rtl';
  R.push(['detached el dir=rtl ctx', e6.getContext('2d').direction]);
  return R;
})()
