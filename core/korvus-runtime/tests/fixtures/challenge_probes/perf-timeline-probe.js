(async () => {
  const R = [];
  const spin = (ms) => { const t = performance.now(); while (performance.now() - t < ms) {} };
  const sleep = (ms) => new Promise(r => setTimeout(r, ms));
  document.body.append('Just a moment...');
  await sleep(300);
  const p = performance.getEntriesByName('first-paint')[0];
  R.push(['paint entry', !!p]);
  R.push(['paint startTime is its presentationTime, a 4 ms multiple', !!p && p.startTime === p.presentationTime && p.startTime % 4 === 0]);
  R.push(['paint paintTime before presentationTime', !!p && p.paintTime > 0 && p.paintTime <= p.presentationTime]);
  R.push(['paint toJSON keys', p ? Object.keys(p.toJSON()).join(',') : '']);
  const before = performance.getEntriesByType('long-animation-frame').length;
  setTimeout(() => spin(80), 0);
  await sleep(300);
  setTimeout(() => { spin(80); document.body.append('x'); }, 0);
  await sleep(300);
  const loafs = performance.getEntriesByType('long-animation-frame').slice(before);
  R.push(['two long animation frames', loafs.length]);
  const [a, b] = loafs;
  // >= 79, not 80: the spin is timed on the 100 us-clamped performance.now(), the LoAF duration is raw (Blink's too), so
  // an 80 ms spin can last 79.9 ms (Chrome 150 3/3 true with this bound, 2026-10-13)
  R.push(['a long task alone', !!a && a.renderStart === 0 && a.styleAndLayoutStart === 0 && a.duration >= 79 && a.blockingDuration >= 29 && a.blockingDuration <= a.duration - 49]);
  R.push(['a long task alone, toJSON keys', a ? Object.keys(a.toJSON()).join(',') : '']);
  R.push(['a long task that rendered', !!b && b.renderStart >= b.startTime + 80 && b.paintTime >= b.renderStart && b.presentationTime >= b.paintTime && b.presentationTime % 4 === 0]);
  R.push(['a long task that rendered, toJSON keys', b ? Object.keys(b.toJSON()).join(',') : '']);
  R.push(['getEntries lists them', performance.getEntries().filter(e => e.entryType === 'long-animation-frame').length >= 2]);
  R.push(['scripts is a frozen array', !!a && Array.isArray(a.scripts) && Object.isFrozen(a.scripts)]);
  R.push(['firstUIEventTimestamp', a ? a.firstUIEventTimestamp : -1]);
  return R;
})()
