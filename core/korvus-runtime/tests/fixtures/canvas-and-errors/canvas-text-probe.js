(() => {
  const H = (d) => { let h = 2166136261; for (let i = 0; i < d.length; i++) { h ^= d[i]; h = Math.imul(h, 16777619); } return (h >>> 0).toString(16); };
  const r = [];
  const cases = [['16px Arial', 'Hwmqxli'], ['16px sans-serif', 'Hwmqxli'], ['16px serif', 'Hwmqxli'], ['8px serif', 'zGmAU6'],
                 ['11px aanotafontaa', 'Ry@H1'], ['16px DejaVu Sans', 'Hwmqxli'], ['16px Liberation Serif', 'Hwmqxli'], ['30px Arial', 'Hw']];
  for (const [font, text] of cases) {
    for (const [how, x, y, top] of [['alphabetic', 2, 18, false], ['alphabetic+0.5', 2.5, 18.5, false], ['top', 2, 4, true]]) {
      for (const stroke of [false, true]) {
        const c = new OffscreenCanvas(80, 24); const g = c.getContext('2d', {willReadFrequently: true});
        g.font = font; if (top) g.textBaseline = 'top';
        if (stroke) g.strokeText(text, x, y); else g.fillText(text, x, y);
        r.push([(stroke ? 'stroke ' : 'fill ') + font + ' ' + how, H(g.getImageData(0, 0, 80, 24).data)]);
      }
    }
  }
  // LCD text: an opaque canvas
  const o = new OffscreenCanvas(80, 24); const og = o.getContext('2d', {alpha: false, willReadFrequently: true});
  og.fillStyle = '#fff'; og.fillRect(0, 0, 80, 24); og.fillStyle = '#000'; og.font = '16px Arial'; og.fillText('Hwmqxli', 2, 18);
  og.strokeStyle = '#06c'; og.font = '11px sans-serif'; og.strokeText('Ry', 60, 20);
  const od = og.getImageData(0, 0, 80, 24).data; let colored = 0; for (let i = 0; i < od.length; i += 4) if (od[i] !== od[i + 1] || od[i + 1] !== od[i + 2]) colored++;
  r.push(['lcd opaque', H(od), colored > 0]);
  return r;
})()
