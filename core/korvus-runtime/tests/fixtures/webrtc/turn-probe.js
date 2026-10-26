(async function () {
  // ICE gathering through a TURN server (`turn-server.py` on 127.0.0.1:34782, user "u", password "p"): the relay
  // candidate a page reads, and the server-reflexive one the Allocate answer's mapped address gives (ABPROG batch 4,
  // BrowserScan N3). A row is [config, type, candidate (foundation, ports, ufrag and mDNS name written as names),
  // relatedAddress, url, relayProtocol, priority, protocol]; then the config's last state and candidate count.
  const out = [];
  const norm = (s) => s.replace(/^candidate:\d+/, 'candidate:F').replace(/ \d+ typ/, ' PORT typ').replace(/rport \d+/, 'rport RP')
    .replace(/ufrag \S+/, 'ufrag U').replace(/[0-9a-f-]{36}\.local/, 'MDNS');
  async function gather(cfg, name) {
    const pc = new RTCPeerConnection(cfg);
    pc.createDataChannel('d');
    const cands = [];
    const errors = [];
    pc.onicecandidateerror = (e) => errors.push([e.errorCode, e.errorText, e.url, e.port === null ? null : 'port', e.address].join('|'));
    const done = new Promise((r) => {
      pc.onicecandidate = (e) => { if (!e.candidate) r('complete'); else cands.push(e.candidate); };
      setTimeout(() => r('timeout'), 8000);
    });
    await pc.setLocalDescription(await pc.createOffer());
    const how = await done;
    for (const c of cands) {
      out.push([name, c.type, norm(c.candidate), c.relatedAddress, c.url, c.relayProtocol, c.priority, c.protocol]);
    }
    out.push([name, how, pc.iceGatheringState, cands.length, errors.join(' ; ')]);
    pc.close();
  }
  const turn = { urls: 'turn:127.0.0.1:34782?transport=udp', username: 'u', credential: 'p' };
  await gather({ iceServers: [turn] }, 'turn');
  await gather({ iceServers: [turn], iceTransportPolicy: 'relay' }, 'relay only');
  await gather({ iceServers: [{ urls: 'turn:127.0.0.1:34782?transport=udp', username: 'u', credential: 'wrong' }], iceTransportPolicy: 'relay' }, 'wrong credential');
  return out;
})()
