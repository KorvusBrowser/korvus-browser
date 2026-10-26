(() => { const R = []; const t = (k, f) => { try { R.push([k, f()]); } catch (e) { R.push([k, 'THROW ' + e.name + ': ' + e.message]); } };
  const m = performance.memory; const p = Object.getPrototypeOf(m);
  t('toString', () => Object.prototype.toString.call(m));
  t('same object', () => performance.memory === performance.memory);
  t('typeof MemoryInfo', () => typeof self.MemoryInfo);
  t('own names', () => Object.getOwnPropertyNames(m));
  t('proto names', () => Object.getOwnPropertyNames(p));
  t('proto syms', () => Object.getOwnPropertySymbols(p).map(String));
  t('proto tag', () => p[Symbol.toStringTag]);
  t('proto proto is Object.prototype', () => Object.getPrototypeOf(p) === Object.prototype);
  t('proto ctor', () => Object.prototype.hasOwnProperty.call(p, 'constructor') ? String(p.constructor).slice(0, 60) : 'none');
  for (const n of ['jsHeapSizeLimit', 'totalJSHeapSize', 'usedJSHeapSize']) { t(n + ' desc', () => { const d = Object.getOwnPropertyDescriptor(p, n); return d ? [typeof d.get, d.get && d.get.name, d.get && d.get.length, typeof d.set, d.enumerable, d.configurable] : 'none'; }); t(n + ' type', () => typeof m[n]); t(n + ' int', () => Number.isInteger(m[n])); }
  t('perf desc', () => { const d = Object.getOwnPropertyDescriptor(Performance.prototype, 'memory'); return d ? [typeof d.get, d.get.name, typeof d.set, d.set && d.set.name, d.enumerable, d.configurable] : 'none'; });
  t('brand', () => { try { return Object.getOwnPropertyDescriptor(p, 'usedJSHeapSize').get.call({}); } catch (e) { return 'THROW ' + e.name + ': ' + e.message; } });
  t('set memory', () => { performance.memory = 5; return typeof performance.memory; });
  t('json', () => JSON.stringify(performance.memory).replace(/\d+/g, 'N'));
  t('keys', () => Object.keys(performance.memory));
  t('forin', () => { const k = []; for (const x in performance.memory) k.push(x); return k; });
  t('circ', () => { const o = performance.memory; o.c = o; try { JSON.stringify(o); } catch (e) { return e.message.split('\n')[1]; } });
  t('limit', () => performance.memory.jsHeapSizeLimit);
  t('ratio used<=total', () => performance.memory.usedJSHeapSize <= performance.memory.totalJSHeapSize);
  return R; })()
