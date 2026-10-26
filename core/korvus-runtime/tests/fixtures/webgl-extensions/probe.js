(() => {
  const R = [];
  const shape = (o) => { if (o === null || typeof o !== 'object') return String(o); const p = Object.getPrototypeOf(o); const ds = Object.getOwnPropertyNames(p).map((n) => { const d = Object.getOwnPropertyDescriptor(p, n); return n + ':' + (d.get ? 'g' : '') + (d.set ? 's' : '') + (typeof d.value === 'function' ? 'f' + d.value.length : d.value !== undefined ? 'v' : '') + (d.enumerable ? 'E' : '') + (d.configurable ? 'C' : '') + (d.writable ? 'W' : ''); }); return Object.prototype.toString.call(o) + ' ' + ds.join(',') + ' ctor=' + (p.constructor && p.constructor.name); };
  for (const ctx of ['webgl', 'webgl2']) { const g = document.createElement('canvas').getContext(ctx); for (const e of g.getSupportedExtensions()) R.push(ctx + ':' + e + ' ' + shape(g.getExtension(e))); }
  return R;
})()
