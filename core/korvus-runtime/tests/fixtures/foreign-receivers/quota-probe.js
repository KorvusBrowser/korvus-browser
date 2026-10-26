(async () => {
  const R = []; const q = navigator.webkitTemporaryStorage; const pq = navigator.webkitPersistentStorage;
  // the realm's first quota calls, and a 0 ms timer set right after them: the order the answers arrive in
  const order = [];
  await new Promise((res) => {
    q.queryUsageAndQuota(function (u, qq) { order.push(['q', typeof u, typeof qq, arguments.length, this === self, Number.isInteger(u), Number.isInteger(qq), u <= qq]); });
    pq.requestQuota(1024, function (g) { order.push(['r', typeof g, g, arguments.length]); });
    pq.requestQuota(-1, function (g) { order.push(['rneg', typeof g, Number.isInteger(g), arguments.length]); });
    q.requestQuota(0, function (g) { order.push(['r0', g]); });
    Promise.resolve().then(() => order.push(['micro']));
    setTimeout(() => order.push(['t0']), 0);
    setTimeout(res, 300);
  });
  R.push(['order', order.map((o) => o[0]).join(',')]);
  R.push(['details', order.filter((o) => o.length > 1)]);
  const t = (k, f) => { try { const v = f(); R.push([k, v === undefined ? 'undefined' : typeof v]); } catch (e) { R.push([k, 'THROW ' + e.name + ': ' + e.message]); } };
  t('q()', () => q.queryUsageAndQuota());
  t('q(1)', () => q.queryUsageAndQuota(1));
  t('q(fn, 1)', () => q.queryUsageAndQuota(() => {}, 1));
  t('q(fn, null)', () => q.queryUsageAndQuota(() => {}, null));
  t('q(null)', () => q.queryUsageAndQuota(null));
  t('r()', () => q.requestQuota());
  t('r(1, 1)', () => q.requestQuota(1, 1));
  t('r(1, null, 2)', () => q.requestQuota(1, null, 2));
  t('r(Symbol())', () => q.requestQuota(Symbol()));
  t('r(10n)', () => q.requestQuota(10n));
  t('r(1)', () => q.requestQuota(1));
  t('same', () => String(q === navigator.webkitTemporaryStorage) + String(Object.getPrototypeOf(q) === Object.getPrototypeOf(pq)) + String(q === pq));
  await new Promise((res) => { const got = []; const h = (e) => { got.push(e.message); e.preventDefault(); }; addEventListener('error', h); q.queryUsageAndQuota(() => { throw new Error('cbthrow'); }); setTimeout(() => { removeEventListener('error', h); R.push(['onerror', got]); res(); }, 200); });
  return R;
})()
