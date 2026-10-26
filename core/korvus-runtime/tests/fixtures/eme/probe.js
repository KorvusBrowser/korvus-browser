(async function () {
  // Encrypted Media Extensions as a page reads them (ABPROG batch 4, a fingerprinting library N14): which key systems and
  // configurations requestMediaKeySystemAccess grants, the configuration it answers, and a ClearKey session's first
  // steps. A row is [case, answer].
  const out = [];
  const conf = (a) => JSON.stringify(a.getConfiguration(), (k, v) => (v === undefined ? 'undefined' : v));
  async function ask(name, ks, configs) {
    try {
      const a = await navigator.requestMediaKeySystemAccess(ks, configs);
      out.push([name, a.keySystem, conf(a)]);
      return a;
    } catch (e) {
      out.push([name, e.name + ': ' + e.message]);
      return null;
    }
  }
  const mp4a = { contentType: 'audio/mp4; codecs="mp4a.40.2"' };
  const avc = { contentType: 'video/mp4; codecs="avc1.42E01E"' };
  const vp9 = { contentType: 'video/webm; codecs="vp9"' };
  for (const ks of ['org.w3.clearkey', 'com.widevine.alpha']) {
    await ask(ks + ' audio cenc', ks, [{ initDataTypes: ['cenc'], audioCapabilities: [mp4a] }]);
    await ask(ks + ' video', ks, [{ videoCapabilities: [avc] }]);
    await ask(ks + ' empty config', ks, [{}]);
    await ask(ks + ' all init types', ks, [{ initDataTypes: ['cenc', 'keyids', 'webm'], videoCapabilities: [avc, vp9] }]);
    await ask(ks + ' persistent', ks, [{ videoCapabilities: [avc], persistentState: 'required', sessionTypes: ['persistent-license'] }]);
    await ask(ks + ' distinctive', ks, [{ videoCapabilities: [avc], distinctiveIdentifier: 'required' }]);
    await ask(ks + ' robustness', ks, [{ videoCapabilities: [{ contentType: avc.contentType, robustness: 'SW_SECURE_DECODE' }] }]);
    await ask(ks + ' unknown codec', ks, [{ videoCapabilities: [{ contentType: 'video/mp4; codecs="bogus"' }] }]);
    await ask(ks + ' second config', ks, [{ videoCapabilities: [{ contentType: 'video/foo' }] }, { audioCapabilities: [mp4a], label: 'b' }]);
  }
  await ask('no configs', 'org.w3.clearkey', []);
  await ask('empty key system', '', [{}]);
  const a = await ask('clearkey for a session', 'org.w3.clearkey', [{ initDataTypes: ['keyids'], videoCapabilities: [avc] }]);
  if (a) {
    const mk = await a.createMediaKeys();
    out.push(['MediaKeys', Object.prototype.toString.call(mk), typeof mk.setServerCertificate, String(await mk.setServerCertificate(new Uint8Array([1])).catch((e) => e.name))]);
    try { mk.createSession('persistent-license'); out.push(['persistent session', 'made']); } catch (e) { out.push(['persistent session', e.name + ': ' + e.message]); }
    const s = mk.createSession();
    out.push(['session', JSON.stringify(s.sessionId), String(s.expiration), s.keyStatuses.size]);
    const msg = new Promise((r) => { s.onmessage = (e) => r([e.messageType, new TextDecoder().decode(e.message)]); setTimeout(() => r(['none']), 1000); });
    const init = new TextEncoder().encode(JSON.stringify({ kids: ['AAAAAAAAAAAAAAAAAAAAAA'] }));
    try {
      await s.generateRequest('keyids', init);
      const m = await msg;
      out.push(['generateRequest keyids', m[0], m[1], s.sessionId.length]);
    } catch (e) {
      out.push(['generateRequest keyids', e.name + ': ' + e.message]);
    }
    try { await mk.createSession().generateRequest('cenc', new Uint8Array(4)); out.push(['generateRequest bad cenc', 'ok']); } catch (e) { out.push(['generateRequest bad cenc', e.name + ': ' + e.message]); }
  }
  return out;
})()
