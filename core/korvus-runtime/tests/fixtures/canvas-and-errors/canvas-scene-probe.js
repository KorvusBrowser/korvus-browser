(() => {
  const H = (d) => { let h = 2166136261; for (let i = 0; i < d.length; i++) { h ^= d[i]; h = Math.imul(h, 16777619); } return (h >>> 0).toString(16); };
  const r = [];
  const grad = (x, a, s0, s1) => { const g = x.createRadialGradient(...a); g.addColorStop(0, s0); g.addColorStop(1, s1); return g; };
  // a bot challenge's own 48x48 scene, as its trace drew it
  for (const [name, attrs] of [['alpha', {willReadFrequently: true}], ['opaque', {willReadFrequently: true, alpha: false}]]) {
    const c = new OffscreenCanvas(48, 48); const x = c.getContext('2d', attrs);
    x.scale(0.384, 0.384);
    x.fillStyle = grad(x, [33, 18, 8, 42, 10, 226], '#809900', '#404041'); x.shadowBlur = 11; x.shadowColor = '#F38020';
    x.beginPath(); x.moveTo(9, 14); x.quadraticCurveTo(93, 48, 116, 111); x.stroke(); x.fill(); x.shadowBlur = 0;
    x.fillStyle = grad(x, [77, 98, 2, 27, 30, 206], '#809900', '#404041');
    x.beginPath(); x.ellipse(58, 55, 31, 28, 1.4441705959829747, 0.5401125108618993, 4.052233984744969); x.stroke(); x.fill(); x.shadowBlur = 0;
    x.fillStyle = grad(x, [108, 12, 10, 65, 118, 169], '#1AB399', '#E666B3'); x.shadowBlur = 16; x.shadowColor = '#809980';
    x.font = '27.77777777777778px aanotafontaa'; x.fillText('Ry', 13, 67); x.shadowBlur = 0;
    x.fillStyle = grad(x, [46, 47, 0, 101, 108, 207], '#4DB380', '#FF4D4D'); x.shadowBlur = 3; x.shadowColor = '#FF6633';
    x.beginPath(); x.moveTo(54, 5); x.bezierCurveTo(54, 90, 32, 74, 71, 120); x.stroke(); x.fill(); x.shadowBlur = 0;
    x.fillStyle = grad(x, [119, 123, 3, 109, 90, 137], '#E6B333', '#3366E6'); x.shadowBlur = 4; x.shadowColor = '#B3B31A';
    x.beginPath(); x.moveTo(76, 0); x.bezierCurveTo(1, 49, 103, 67, 49, 125); x.stroke(); x.fill(); x.shadowBlur = 0;
    x.fillStyle = grad(x, [34, 47, 1, 37, 59, 245], '#809900', '#404041');
    x.beginPath(); x.ellipse(56, 57, 14, 8, 1.2273132071162383, 4.1926143018618225, 2.8853539230051624); x.stroke(); x.fill(); x.shadowBlur = 0;
    x.shadowBlur = 14; x.shadowColor = '#809900'; x.font = '11.904761904761905px aanotafontaa'; x.strokeText('@H1', 30, 73); x.shadowBlur = 0;
    r.push(['scene ' + name, H(x.getImageData(0, 0, 48, 48).data)]);
  }
  return r;
})()
