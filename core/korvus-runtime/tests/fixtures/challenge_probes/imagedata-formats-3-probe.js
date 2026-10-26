(() => {
  const out = [];
  const T = (k, f) => { try { out.push([k, f()]); } catch (e) { out.push([k, 'THROW ' + e.name + ': ' + e.message]); } };
  T('u8 own', () => Object.getOwnPropertyNames(new ImageData(3, 2)).join(','));
  T('u8 own from data', () => Object.getOwnPropertyNames(new ImageData(new Uint8ClampedArray(8), 2)).join(','));
  T('get own', () => { const c = document.createElement('canvas').getContext('2d'); return Object.getOwnPropertyNames(c.getImageData(0, 0, 1, 1)).join(','); });
  T('proto data desc', () => { const d = Object.getOwnPropertyDescriptor(ImageData.prototype, 'data'); return d ? Object.keys(d).join(',') + (d.get ? ' getter' : '') : 'none'; });
  T('f32 fmt', () => { const d = new ImageData(1, 1, { pixelFormat: 'rgba-float32' }); return [d.pixelFormat, d.data.constructor.name]; });
  T('data identity', () => { const d = new ImageData(1, 1); return d.data === d.data; });
  T('3 args undefined', () => new ImageData(new Uint8ClampedArray(8), 2, undefined).height);
  T('4 args undefined', () => new ImageData(new Uint8ClampedArray(8), 2, undefined, {}).height);
  T('int8 data', () => new ImageData(new Int8Array(8), 2));
  return out;
})()
