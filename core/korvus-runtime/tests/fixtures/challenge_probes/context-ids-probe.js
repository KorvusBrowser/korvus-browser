(() => {
  const R = [];
  const t = (name, f) => { try { const o = f(); R.push([name, o === null ? 'null' : o === undefined ? 'undefined' : Object.prototype.toString.call(o)]); } catch (e) { R.push([name, e.name + ': ' + e.message]); } };
  for (const id of ['2D', ' 2d', '2d ', 'WebGL', 'experimental-webgl', 'webgpu', 'bitmaprenderer', '', 'foo', 2, null, undefined]) {
    t('oc ' + String(id), () => new OffscreenCanvas(5, 5).getContext(id));
    t('el ' + String(id), () => document.createElement('canvas').getContext(id));
  }
  t('oc noarg', () => new OffscreenCanvas(5, 5).getContext());
  t('el noarg', () => document.createElement('canvas').getContext());
  t('oc toString throws', () => new OffscreenCanvas(5, 5).getContext({ toString() { throw new RangeError('x'); } }));
  t('el toString throws', () => document.createElement('canvas').getContext({ toString() { throw new RangeError('x'); } }));
  t('oc 2d then webgl', () => { const c = new OffscreenCanvas(5, 5); c.getContext('2d'); return c.getContext('webgl'); });
  t('oc 2d then foo', () => { const c = new OffscreenCanvas(5, 5); c.getContext('2d'); return c.getContext('foo'); });
  t('oc 2d then bitmaprenderer', () => { const c = new OffscreenCanvas(5, 5); c.getContext('2d'); return c.getContext('bitmaprenderer'); });
  t('oc bad attrs', () => new OffscreenCanvas(5, 5).getContext('2d', 5));
  t('oc attrs getter throws', () => new OffscreenCanvas(5, 5).getContext('2d', { get alpha() { throw new SyntaxError('y'); } }));
  t('oc invalid id attrs getter', () => { let n = 0; try { new OffscreenCanvas(5, 5).getContext('foo', { get alpha() { n++; return true; } }); } catch (e) { return { toString() { return 'x' } } } return null; });
  let order = [];
  try { new OffscreenCanvas(5, 5).getContext({ toString() { order.push('id'); return 'foo'; } }, { get alpha() { order.push('alpha'); return true; } }); } catch (e) { order.push(e.name); }
  R.push(['oc order invalid', order.join(',')]);
  order = [];
  try { document.createElement('canvas').getContext({ toString() { order.push('id'); return 'foo'; } }, { get alpha() { order.push('alpha'); return true; } }); } catch (e) { order.push(e.name); }
  R.push(['el order invalid', order.join(',')]);
  order = [];
  try { document.createElement('canvas').getContext({ toString() { order.push('id'); return '2d'; } }, { get alpha() { order.push('alpha'); return true; } }); } catch (e) { order.push(e.name); }
  R.push(['el order 2d', order.join(',')]);
  order = [];
  try { document.createElement('canvas').getContext({ toString() { order.push('id'); return 'webgl'; } }, { get alpha() { order.push('alpha'); return true; } }); } catch (e) { order.push(e.name); }
  R.push(['el order webgl', order.join(',')]);
  order = [];
  try { new OffscreenCanvas(5,5).getContext({ toString() { order.push('id'); return '2d'; } }, { get alpha() { order.push('alpha'); return true; } }); } catch (e) { order.push(e.name); }
  R.push(['oc order 2d', order.join(',')]);
  return R;
})()
