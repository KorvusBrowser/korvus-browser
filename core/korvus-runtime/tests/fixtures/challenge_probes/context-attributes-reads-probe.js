(() => {
  const R = [];
  const logged = (log) => new Proxy({}, { get(t, k) { log.push(String(k)); return undefined; }, has(t, k) { log.push('has:' + String(k)); return false; } });
  for (const id of ['2d', 'webgl', 'webgl2', 'bitmaprenderer', 'webgpu', 'foo']) {
    for (const host of ['el', 'oc']) {
      const log = [];
      let res;
      try { const c = host === 'el' ? document.createElement('canvas') : new OffscreenCanvas(5, 5); const o = c.getContext(id, logged(log)); res = o === null ? 'null' : Object.prototype.toString.call(o); } catch (e) { res = e.name; }
      R.push([host + ' ' + id, res + ' | ' + log.join(',')]);
    }
  }
  // second call on an existing context: are options read again?
  for (const host of ['el', 'oc']) {
    const c = host === 'el' ? document.createElement('canvas') : new OffscreenCanvas(5, 5);
    c.getContext('2d');
    const log = []; c.getContext('2d', logged(log)); R.push([host + ' 2d again', log.join(',')]);
    const log2 = []; c.getContext('webgl', logged(log2)); R.push([host + ' webgl after 2d', log2.join(',')]);
  }
  return R;
})()
