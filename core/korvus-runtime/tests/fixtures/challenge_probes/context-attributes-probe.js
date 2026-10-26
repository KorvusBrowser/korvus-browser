(() => {
  const R = [];
  const show = (o) => o === null ? 'null' : Object.prototype.toString.call(o);
  const t = (name, f) => { try { R.push([name, f()]); } catch (e) { R.push([name, e.name + ': ' + e.message]); } };
  const cases = [
    ['colorSpace foo', { colorSpace: 'foo' }], ['colorSpace srgb-linear', { colorSpace: 'srgb-linear' }], ['colorSpace rec2020', { colorSpace: 'rec2020' }],
    ['colorSpace display-p3-linear', { colorSpace: 'display-p3-linear' }], ['colorSpace rec2100-linear', { colorSpace: 'rec2100-linear' }], ['colorSpace rec2100-pq', { colorSpace: 'rec2100-pq' }],
    ['colorType foo', { colorType: 'foo' }], ['colorType float16', { colorType: 'float16' }],
    ['wrf 1', { willReadFrequently: 1 }], ['wrf true', { willReadFrequently: true }], ['wrf "true"', { willReadFrequently: 'true' }], ['wrf false', { willReadFrequently: false }],
    ['wrf {}', { willReadFrequently: {} }], ['wrf null', { willReadFrequently: null }], ['wrf "undefined"', { willReadFrequently: 'undefined' }],
    ['pp foo', { powerPreference: 'foo' }], ['pp low-power', { powerPreference: 'low-power' }], ['pp 1', { powerPreference: 1 }],
    ['alpha 0', { alpha: 0 }], ['desync 1', { desynchronized: 1 }],
    ['colorSpace foo + colorType foo', { colorSpace: 'foo', colorType: 'foo' }],
    ['srgb-linear + colorType foo', { colorSpace: 'srgb-linear', colorType: 'foo' }],
  ];
  for (const [name, a] of cases) {
    for (const host of ['el', 'oc']) {
      for (const id of ['2d', 'webgl', 'foo']) {
        if (host === 'oc' && id === 'foo') continue;
        t(host + ' ' + id + ' ' + name, () => {
          const c = host === 'el' ? document.createElement('canvas') : new OffscreenCanvas(5, 5);
          const o = c.getContext(id, a);
          if (o && id === '2d') return show(o) + ' ' + JSON.stringify(o.getContextAttributes());
          if (o && id === 'webgl') return show(o) + ' ' + JSON.stringify(o.getContextAttributes());
          return show(o);
        });
      }
    }
  }
  t('el 2d existing + bad attrs', () => { const c = document.createElement('canvas'); c.getContext('2d'); return show(c.getContext('2d', { colorSpace: 'foo' })); });
  t('el 2d existing + srgb-linear', () => { const c = document.createElement('canvas'); c.getContext('2d'); return show(c.getContext('2d', { colorSpace: 'srgb-linear' })); });
  t('el 2d existing + p3', () => { const c = document.createElement('canvas'); const a = c.getContext('2d'); const b = c.getContext('2d', { colorSpace: 'display-p3' }); return String(a === b) + ' ' + JSON.stringify(b.getContextAttributes()); });
  t('el transferred getContext', () => { const c = document.createElement('canvas'); c.transferControlToOffscreen(); return show(c.getContext('2d')); });
  t('el transferred getContext foo', () => { const c = document.createElement('canvas'); c.transferControlToOffscreen(); return show(c.getContext('foo')); });
  t('el transferred getContext bad attrs', () => { const c = document.createElement('canvas'); c.transferControlToOffscreen(); return show(c.getContext('2d', { colorSpace: 'foo' })); });
  t('el transferred getContext srgb-linear', () => { const c = document.createElement('canvas'); c.transferControlToOffscreen(); return show(c.getContext('2d', { colorSpace: 'srgb-linear' })); });
  t('oc webgpu canvas', () => { const c = new OffscreenCanvas(5, 5); const g = c.getContext('webgpu'); return g ? String(g.canvas === c) + ' ' + JSON.stringify(Object.getOwnPropertyNames(g)) : 'null'; });
  t('oc webgpu then 2d', () => { const c = new OffscreenCanvas(5, 5); c.getContext('webgpu'); return show(c.getContext('2d')); });
  t('oc 2d then webgpu', () => { const c = new OffscreenCanvas(5, 5); c.getContext('2d'); return show(c.getContext('webgpu')); });
  return R;
})()
