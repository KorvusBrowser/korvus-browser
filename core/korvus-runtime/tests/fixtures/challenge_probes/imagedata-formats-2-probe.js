(() => {
  const out = [];
  const T = (k, f) => { try { const v = f(); out.push([k, v]); } catch (e) { out.push([k, 'THROW ' + e.name + ': ' + e.message]); } };
  T('zero w + bad fmt', () => new ImageData(0, 1, { pixelFormat: 'foo' }));
  T('zero w + bad cs', () => new ImageData(0, 1, { colorSpace: 'foo' }));
  T('bad cs', () => new ImageData(1, 1, { colorSpace: 'foo' }));
  T('f16 len5', () => new ImageData(new Float16Array(5), 1));
  T('f16 len5 fmt', () => new ImageData(new Float16Array(5), 1, undefined, { pixelFormat: 'rgba-float16' }));
  T('f16 len0', () => new ImageData(new Float16Array(0), 1));
  T('f16 h mismatch fmt', () => new ImageData(new Float16Array(8), 1, 3, { pixelFormat: 'rgba-float16' }));
  T('f16 w zero', () => new ImageData(new Float16Array(8), 0));
  T('u8 f16 fmt len5', () => new ImageData(new Uint8ClampedArray(5), 1, undefined, { pixelFormat: 'rgba-float16' }));
  T('f32 array', () => new ImageData(new Float32Array(8), 2));
  T('f16 own data', () => { const d = new ImageData(1, 1, { pixelFormat: 'rgba-float16' }); return [Object.getOwnPropertyNames(d).join(','), d.data === d.data, d.data.byteLength]; });
  T('f16 shared buffer', () => { const a = new Float16Array(4); const d = new ImageData(a, 1, 1, { pixelFormat: 'rgba-float16' }); return d.data === a; });
  const c = document.createElement('canvas'); c.width = 2; c.height = 1; const x = c.getContext('2d');
  T('round', () => { const vals = []; for (const b of [0, 1, 2, 63, 64, 127, 128, 129, 191, 254, 255]) { x.clearRect(0, 0, 2, 1); x.fillStyle = `rgb(${b},${b},${b})`; x.fillRect(0, 0, 1, 1); vals.push(x.getImageData(0, 0, 1, 1, { pixelFormat: 'rgba-float16' }).data[0]); } return vals; });
  T('alpha', () => { x.clearRect(0, 0, 2, 1); x.fillStyle = 'rgba(200, 100, 50, 0.3)'; x.fillRect(0, 0, 1, 1); return [Array.from(x.getImageData(0, 0, 1, 1).data), Array.from(x.getImageData(0, 0, 1, 1, { pixelFormat: 'rgba-float16' }).data)]; });
  T('put back', () => { const vals = []; for (const v of [0, 0.001, 0.002, 0.25, 0.4999, 0.5, 0.5001, 0.75, 0.999, 1, 1.5, -0.5]) { const d = new ImageData(new Float16Array([v, v, v, 1]), 1, 1, { pixelFormat: 'rgba-float16' }); x.putImageData(d, 0, 0); vals.push(x.getImageData(0, 0, 1, 1).data[0]); } return vals; });
  T('put alpha', () => { const d = new ImageData(new Float16Array([1, 0.5, 0.25, 0.5]), 1, 1, { pixelFormat: 'rgba-float16' }); x.putImageData(d, 0, 0); return Array.from(x.getImageData(0, 0, 1, 1).data); });
  T('get bad fmt', () => x.getImageData(0, 0, 1, 1, { pixelFormat: 'x' }));
  T('create bad fmt', () => x.createImageData(1, 1, { pixelFormat: 'x' }));
  T('p3 f16', () => { const d = x.getImageData(0, 0, 1, 1, { colorSpace: 'display-p3', pixelFormat: 'rgba-float16' }); return [d.colorSpace, d.pixelFormat, Array.from(d.data)]; });
  return out;
})()
