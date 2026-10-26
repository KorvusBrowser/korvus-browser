(async () => {
  const out = {};
  const load = (src) => new Promise(res => { const i = new Image(); i.onload = () => res(['load', i]); i.onerror = () => res(['error', i]); i.src = src; });
  for (const f of ['ci1.png', 'good.png', 'trunc.png', 'notpng.png']) {
    const r = {};
    const pre = new Image(); pre.src = f;
    const early = pre.decode().then(() => 'resolved ' + pre.naturalWidth, e => 'rejected ' + e.name + ': ' + e.message);
    const [ev, img] = await load(f);
    r.ev = ev; r.size = [img.naturalWidth, img.naturalHeight, img.width, img.complete];
    r.earlyDecode = await early;
    r.lateDecode = await img.decode().then(() => 'resolved', e => 'rejected ' + e.name + ': ' + e.message);
    try { const b = await createImageBitmap(img); r.bitmapImg = [b.width, b.height]; } catch (e) { r.bitmapImg = 'THROW ' + e.name + ': ' + e.message; }
    try { const c = document.createElement('canvas'); c.width = 4; c.height = 4; const x = c.getContext('2d'); x.drawImage(img, 0, 0); r.draw = Array.from(x.getImageData(0, 0, 2, 1).data).join(','); } catch (e) { r.draw = 'THROW ' + e.name + ': ' + e.message; }
    try { const c = document.createElement('canvas'); const x = c.getContext('2d'); r.pattern = String(x.createPattern(img, 'repeat')); } catch (e) { r.pattern = 'THROW ' + e.name + ': ' + e.message; }
    try { const b = await createImageBitmap(await (await fetch(f)).blob()); r.bitmapBlob = [b.width, b.height]; } catch (e) { r.bitmapBlob = 'THROW ' + e.name + ': ' + e.message; }
    out[f] = r;
  }
  return out;
})()
