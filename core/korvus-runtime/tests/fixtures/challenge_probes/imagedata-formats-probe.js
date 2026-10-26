(() => {
  const out = [];
  const T = (k, f) => { try { const v = f(); out.push([k, v]); } catch (e) { out.push([k, 'THROW ' + e.name + ': ' + e.message]); } };
  T('ctor f16', () => { const d = new ImageData(2, 1, { pixelFormat: 'rgba-float16' }); return [d.pixelFormat, Object.prototype.toString.call(d.data), d.data.length, d.data.constructor.name, d.colorSpace]; });
  T('ctor u8', () => { const d = new ImageData(2, 1, { pixelFormat: 'rgba-unorm8' }); return [d.pixelFormat, Object.prototype.toString.call(d.data)]; });
  T('ctor bad', () => new ImageData(2, 1, { pixelFormat: 'foo' }).pixelFormat);
  T('ctor from f16 array', () => { const d = new ImageData(new Float16Array(8), 2, 1, { pixelFormat: 'rgba-float16' }); return [d.pixelFormat, d.data.length]; });
  T('ctor from f16 array no fmt', () => { const d = new ImageData(new Float16Array(8), 2); return [d.pixelFormat]; });
  T('ctor from u8 with f16 fmt', () => new ImageData(new Uint8ClampedArray(8), 2, 1, { pixelFormat: 'rgba-float16' }).pixelFormat);
  const c = document.createElement('canvas'); c.width = 4; c.height = 4; const x = c.getContext('2d');
  x.fillStyle = 'rgb(255, 128, 0)'; x.fillRect(0, 0, 4, 4); x.fillStyle = 'rgba(0, 0, 255, 0.5)'; x.fillRect(1, 1, 2, 2);
  T('get f16', () => { const d = x.getImageData(0, 0, 2, 2, { pixelFormat: 'rgba-float16' }); return [d.pixelFormat, d.data.constructor.name, Array.from(d.data.slice(0, 8)), Array.from(d.data.slice(12, 16)), Array.from(new Uint8Array(d.data.buffer).slice(0, 8))]; });
  T('get u8', () => { const d = x.getImageData(0, 0, 2, 2); return [d.pixelFormat, Array.from(d.data.slice(12, 16))]; });
  T('create f16', () => { const d = x.createImageData(2, 2, { pixelFormat: 'rgba-float16' }); return [d.pixelFormat, d.data.constructor.name]; });
  T('create from f16', () => { const s = new ImageData(2, 1, { pixelFormat: 'rgba-float16' }); const d = x.createImageData(s); return [d.pixelFormat, d.data.constructor.name]; });
  T('put f16', () => { const d = new ImageData(new Float16Array([1, 0, 0, 1, 0, 0.5, 0, 1]), 2, 1, { pixelFormat: 'rgba-float16' }); x.putImageData(d, 0, 0); return Array.from(x.getImageData(0, 0, 2, 1).data); });
  const oc = new OffscreenCanvas(4, 4); const ox = oc.getContext('2d'); ox.fillStyle = '#336699'; ox.fillRect(0, 0, 4, 4);
  T('offscreen get f16', () => { const d = ox.getImageData(0, 0, 1, 1, { pixelFormat: 'rgba-float16' }); return [d.pixelFormat, Array.from(d.data)]; });
  T('settings dict order', () => { const seen = []; const o = {}; for (const k of ['colorSpace', 'pixelFormat']) Object.defineProperty(o, k, { get() { seen.push(k); return undefined; } }); new ImageData(1, 1, o); return seen.join(','); });
  T('Float16Array', () => typeof Float16Array);
  return out;
})()
