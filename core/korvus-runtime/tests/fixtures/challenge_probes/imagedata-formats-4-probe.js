(() => {
  const out = [];
  for (const cs of ['srgb', 'rec2020', 'display-p3', 'rec2100-hlg', 'rec2100-pq', 'srgb-linear', 'display-p3-linear', 'rec2100-linear', 'bogus']) {
    try { const d = new ImageData(1, 1, { colorSpace: cs }); out.push([cs, d.colorSpace]); } catch (e) { out.push([cs, 'THROW ' + e.name + ': ' + e.message]); }
    try { const c = document.createElement('canvas').getContext('2d'); const d = c.getImageData(0, 0, 1, 1, { colorSpace: cs }); out.push(['get ' + cs, d.colorSpace]); } catch (e) { out.push(['get ' + cs, 'THROW ' + e.name + ': ' + e.message]); }
  }
  return out;
})()
