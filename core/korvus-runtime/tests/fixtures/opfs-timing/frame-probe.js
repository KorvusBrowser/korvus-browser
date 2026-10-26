// The challenge's OPFS step, timed (an interactive-widget challenge 2026-10-10): run in a CROSS-SITE frame, the storage key's first calls,
// then a worker's sync access handles. Every value is milliseconds of performance.now() around one awaited call;
// `min` keeps the fastest of three tries (a loaded machine only ever adds time).
(async () => {
  const r = {};
  const ms = async (f) => { const a = performance.now(); await f(); return +(performance.now() - a).toFixed(1); };
  let root;
  r.getDirectory = await ms(async () => { root = await navigator.storage.getDirectory(); });
  r.firstOperation = await ms(() => navigator.storage.estimate());
  r.getDirectoryAgain = await ms(() => navigator.storage.getDirectory());
  for (const n of ['a', 'b', 'c']) await root.getFileHandle(n, {create: true});
  const keys = [];
  for (let i = 0; i < 3; i++) keys.push(await ms(async () => { for await (const k of root.keys()) {} }));
  r.keysMin = Math.min(...keys);
  const lookups = [];
  for (let i = 0; i < 3; i++) lookups.push(await ms(async () => { for (let j = 0; j < 10; j++) await root.getFileHandle('a'); }));
  r.tenLookupsMin = Math.min(...lookups);
  const src = `onmessage = async () => {
    const ms = async (f) => { const a = performance.now(); await f(); return +(performance.now() - a).toFixed(1); };
    const root = await navigator.storage.getDirectory();
    const handle = [], flushAgain = [];
    for (const n of ['w1', 'w2', 'w3']) {
      const fh = await root.getFileHandle(n, {create: true});
      let h;
      handle.push(await ms(async () => { h = await fh.createSyncAccessHandle(); }));
      h.write(new Uint8Array([1]), {at: 0}); h.flush();
      const a = performance.now(); h.flush(); flushAgain.push(+(performance.now() - a).toFixed(1));
      h.close();
    }
    const tens = [];
    for (let k = 0; k < 3; k++) {
      let ten = 0;
      for (let i = 0; i < 10; i++) {
        const fh = await root.getFileHandle('t' + k + '-' + i, {create: true});
        let h;
        ten += await ms(async () => { h = await fh.createSyncAccessHandle(); });
        h.close();
      }
      tens.push(+ten.toFixed(1));
    }
    postMessage({syncHandleMin: Math.min(...handle), flushNothingNewMin: Math.min(...flushAgain), tenSyncHandlesMin: Math.min(...tens)});
  };`;
  const w = new Worker(URL.createObjectURL(new Blob([src], {type: 'text/javascript'})));
  Object.assign(r, await new Promise((res) => { w.onmessage = (e) => res(e.data); w.postMessage(0); }));
  w.terminate();
  return r;
})()
