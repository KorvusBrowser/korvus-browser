// TEMPLATES P6 B16: a ReadableStream's async iterator, as Chrome builds it (shape and semantics).
async function probeIter() {
  const out = {};
  const t = async (k, f) => { try { const v = await f(); out[k] = v === undefined ? 'undefined' : JSON.stringify(v); } catch (e) { out[k] = 'ERR ' + e.constructor.name + ': ' + e.message; } };
  const mk = (chunks, extra) => new ReadableStream(Object.assign({ start(c) { for (const x of chunks) c.enqueue(x); if (!extra || !extra.open) c.close(); } }, extra || {}));
  const it0 = mk([1]).values(), P = Object.getPrototypeOf(it0);
  const desc = (o, k) => { const d = Object.getOwnPropertyDescriptor(o, k); return d ? [('value' in d) ? typeof d.value : 'acc', d.writable, d.enumerable, d.configurable].join() : 'none'; };
  out.own = Reflect.ownKeys(it0).map(String).join();
  out.protoKeys = Reflect.ownKeys(P).map(String).join();
  out.descs = [desc(P, 'next'), desc(P, 'return'), desc(P, Symbol.toStringTag)].join(' / ');
  out.tag = Object.prototype.toString.call(it0);
  out.shared = mk([1]).values().next === it0.next && Object.getPrototypeOf(mk([2]).values()) === P;
  out.chain = Object.getPrototypeOf(P) === Object.getPrototypeOf(Object.getPrototypeOf(async function* () {}).prototype);
  out.fns = [P.next.name, P.next.length, P.return.name, P.return.length].join();
  out.selfIter = it0[Symbol.asyncIterator]() === it0;
  await t('nextObj', () => P.next.call({}));
  await t('returnObj', () => P.return.call({}));
  await t('seq', async () => { const it = mk([1, 2]).values(); return [await it.next(), await it.next(), await it.next(), await it.next()]; });
  await t('lockedAtEnd', async () => { const rs = mk([1]); const it = rs.values(); await it.next(); const a = rs.locked; await it.next(); return [a, rs.locked]; });
  await t('errored', async () => { let ctl; const rs = new ReadableStream({ start(c) { ctl = c; } }); const it = rs.values(); const p = it.next(); ctl.error(new Error('boom')); let r; try { await p; r = 'resolved'; } catch (e) { r = 'rejected ' + e.message; } return [r, rs.locked]; });
  await t('ret', async () => { let c = 'no'; const rs = mk([1, 2], { open: true, cancel(r) { c = 'cancel:' + r; } }); const it = rs.values(); const r = await it.return('why'); return [r, c, rs.locked, await it.next()]; });
  await t('retPrevent', async () => { let c = 'no'; const rs = mk([1], { open: true, cancel() { c = 'called'; } }); const it = rs.values({ preventCancel: true }); return [await it.return('x'), c, rs.locked]; });
  await t('retTwice', async () => { const it = mk([1], { open: true }).values(); return [await it.return(1), await it.return(2)]; });
  await t('retNoArg', async () => { const it = mk([1], { open: true }).values(); const r = await it.return(); return [Object.keys(r), r.done, r.value === undefined]; });
  await t('breakCancels', async () => { let c = 'no'; const rs = new ReadableStream({ start(ct) { ct.enqueue(1); ct.enqueue(2); }, cancel(r) { c = 'cancel:' + String(r); } }); for await (const x of rs) break; return [c, rs.locked]; });
  await t('forAwait', async () => { const got = []; for await (const x of mk([7, 8])) got.push(x); return got; });
  await t('opt5', () => { mk([1]).values(5); return 'ok'; });
  await t('optNull', () => { mk([1]).values(null); return 'ok'; });
  await t('lockedValues', () => { const r = mk([1]); r.getReader(); return r.values(); });
  return JSON.stringify(out);
}
