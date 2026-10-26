(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN; storage.getDirectory is
  // chrome_opfs_timing_live's (Chrome 150 answers it after the 8 ms timer in one run and the 16 ms one in the next)
  const SKIP = ['startRendering (challenge-like)', 'worker createSyncAccessHandle', 'gpu.requestAdapter', 'Summarizer.availability', 'LanguageModel.availability', 'storage.estimate', 'getFileHandle', 'setLocalDescription', 'storage.getDirectory'];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const race = async (name, start) => {
    const order = [];
    let p;
    try { p = start(); } catch (e) { R.push([name, 'sync ' + e.name + ': ' + e.message]); return; }
    if (!p || typeof p.then !== 'function') { R.push([name, 'not a promise: ' + String(p)]); return; }
    let val;
    p.then((v) => { val = v; order.push('API'); }, (e) => { val = e && e.name; order.push('API!'); });
    Promise.resolve().then(() => order.push('micro'));
    const mc = new MessageChannel(); mc.port1.onmessage = () => order.push('msg'); mc.port2.postMessage(0);
    for (const ms of [0, 1, 2, 4, 8, 16, 32, 64]) setTimeout(() => order.push('t' + ms), ms);
    await wait(150);
    const i = Math.max(order.indexOf('API'), order.indexOf('API!'));
    R.push([name, (i < 0 ? 'pending' : order.slice(0, i + 1).join(' ')) + ' = ' + (val === undefined ? 'undefined' : typeof val === 'object' && val !== null ? Object.prototype.toString.call(val) : String(val))]);
  };
  await race('gpu.requestAdapter', () => navigator.gpu.requestAdapter());
  await race('Summarizer.availability', () => self.Summarizer ? Summarizer.availability() : null);
  await race('LanguageModel.availability', () => self.LanguageModel ? LanguageModel.availability() : null);
  await race('storage.getDirectory', () => navigator.storage.getDirectory());
  await race('getFileHandle', async () => { const d = await navigator.storage.getDirectory(); return d.getFileHandle('x' + Math.random(), { create: true }); });
  await race('getHighEntropyValues', () => navigator.userAgentData.getHighEntropyValues(['architecture', 'bitness', 'model', 'platformVersion', 'fullVersionList', 'wow64']));
  await race('createOffer', () => { const pc = new RTCPeerConnection(); pc.createDataChannel('x'); return pc.createOffer(); });
  await race('setLocalDescription', async () => { const pc = new RTCPeerConnection(); pc.createDataChannel('x'); const o = await pc.createOffer(); return pc.setLocalDescription(o); });
  await race('startRendering (challenge-like)', () => { const c = new OfflineAudioContext(1, 5000, 44100); const o = c.createOscillator(); o.type = 'triangle'; o.frequency.value = 10000; const k = c.createDynamicsCompressor(); o.connect(k); k.connect(c.destination); o.start(0); return c.startRendering(); });
  await race('permissions.query', () => navigator.permissions.query({ name: 'notifications' }));
  await race('storage.estimate', () => navigator.storage.estimate());
  await race('keyboard.getLayoutMap', () => navigator.keyboard.getLayoutMap());
  await race('worker createSyncAccessHandle', () => new Promise((resolve, reject) => {
    const src = 'onmessage=async()=>{try{const d=await navigator.storage.getDirectory();const f=await d.getFileHandle("y"+Math.random(),{create:true});const h=await f.createSyncAccessHandle();postMessage("ok "+Object.prototype.toString.call(h));h.close();}catch(e){postMessage("err "+e.name)}}';
    const w = new Worker(URL.createObjectURL(new Blob([src]))); w.onmessage = (e) => resolve(e.data); w.postMessage(0); }));
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
