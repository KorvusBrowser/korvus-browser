(async function () {
  /* createOffer's result, the descriptions a connection holds, RTCIceCandidate.toJSON / usernameFragment, and the
     MediaStream of a MediaStreamAudioDestinationNode. Ids are reported by shape (they are fresh UUIDs). */
  var out = [];
  var J = function (o) { try { return JSON.stringify(o); } catch (e) { return 'ERR ' + e; } };
  var row = async function (name, f) {
    try { var v = await f(); out.push([name, v]); } catch (e) { out.push([name, 'THROW ' + (e && e.name) + ': ' + (e && e.message)]); }
  };
  var P = function (o) { var p = Object.getPrototypeOf(o); return p === Object.prototype ? 'Object.prototype' : String(p && p.constructor && p.constructor.name); };
  var uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  var mask = function (s, id) { return s.split(id).join('<deviceId>'); };
  await row('createOffer', async function () { var pc = new RTCPeerConnection(); var o = await pc.createOffer({ offerToReceiveAudio: 1 }); pc.close(); return [P(o), Object.keys(o).join(','), o.type, typeof o.sdp]; });
  await row('createOffer, data channel', async function () { var pc = new RTCPeerConnection(); pc.createDataChannel('d'); var o = await pc.createOffer(); pc.close(); return [P(o), Object.keys(o).join(','), o.type]; });
  await row('localDescription', async function () {
    var pc = new RTCPeerConnection(); pc.createDataChannel('d'); var o = await pc.createOffer(); await pc.setLocalDescription(o);
    var d = pc.localDescription; var j = d.toJSON(); var r = [Object.prototype.toString.call(d), d instanceof RTCSessionDescription, Object.getOwnPropertyNames(d).join(','), Object.keys(j).join(','), P(j), j.type, d.sdp === o.sdp, pc.localDescription === d, pc.pendingLocalDescription === d];
    pc.close(); return r;
  });
  var cands = [
    { candidate: '', sdpMid: '0' },
    { candidate: '', sdpMLineIndex: 0 },
    { candidate: '', sdpMid: '0', sdpMLineIndex: 1, usernameFragment: 'abc' },
    { candidate: 'candidate:1 1 udp 2122260223 192.168.1.2 54321 typ host generation 0 ufrag Xy12 network-id 1', sdpMid: '0' },
    { candidate: 'candidate:1 1 udp 2122260223 192.168.1.2 54321 typ host generation 0 ufrag Xy12', sdpMid: '0', usernameFragment: 'other' },
    { candidate: 'candidate:1 1 udp 2122260223 192.168.1.2 54321 typ host', sdpMid: null, sdpMLineIndex: 3 },
    { candidate: 'a=candidate:1 1 udp 2122260223 192.168.1.2 54321 typ host', sdpMid: 'x' }
  ];
  cands.forEach(function (init, i) {
    out.push(['ice ' + i + ' toJSON', (function () { try { var c = new RTCIceCandidate(init); var j = c.toJSON(); return [P(j), J(j)]; } catch (e) { return 'THROW ' + e.name + ': ' + e.message; } })()]);
    out.push(['ice ' + i + ' attrs', (function () { try { var c = new RTCIceCandidate(init); return J([c.candidate, c.sdpMid, c.sdpMLineIndex, c.usernameFragment, c.foundation, c.component, c.priority, c.address, c.protocol, c.port, c.type]); } catch (e) { return 'THROW ' + e.name + ': ' + e.message; } })()]);
  });
  var ac = new AudioContext();
  var dest = ac.createMediaStreamDestination();
  var st = dest.stream;
  await row('stream', function () { return [Object.prototype.toString.call(st), st === dest.stream, st.active, st.getTracks().length, st.getAudioTracks().length, st.getVideoTracks().length, uuid.test(st.id)]; });
  var tr = st.getAudioTracks()[0];
  var id = (tr && tr.getSettings().deviceId) || '';
  await row('track', function () { return [Object.prototype.toString.call(tr), tr.kind, tr.label, uuid.test(tr.id), tr.enabled, tr.muted, tr.readyState, tr.contentHint, st.getTrackById(tr.id) === tr, Object.getOwnPropertyNames(tr).join(',')]; });
  await row('deviceId shape', function () { return /^WebAudio-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(id); });
  await row('getSettings', function () { var s = tr.getSettings(); return [P(s), mask(J(s), id), s.sampleRate === ac.sampleRate, s.latency === Math.floor(ac.sampleRate / 100) / ac.sampleRate]; });
  await row('getCapabilities', function () { var s = tr.getCapabilities(); return [P(s), mask(J(s), id), tr.getCapabilities() !== s, tr.getCapabilities().sampleSize !== s.sampleSize]; });
  await row('getConstraints', function () { var s = tr.getConstraints(); return [P(s), J(s)]; });
  await row('clone', function () { var c = tr.clone(); return [c.id === tr.id, uuid.test(c.id), c.kind, c.label, c.readyState, mask(J(c.getSettings()), id), mask(J(c.getCapabilities()), id)]; });
  await row('stream clone', function () { var c = st.clone(); return [c.id === st.id, c.getTracks().length, c.getTracks()[0].id === tr.id, c.getTracks()[0].label]; });
  await row('applyConstraints({})', async function () { try { await tr.applyConstraints({}); return 'resolved'; } catch (e) { return e.name + ': ' + e.message; } });
  await row('stop', function () { tr.stop(); return [tr.readyState, st.active, mask(J(tr.getSettings()), id)]; });
  await row('second node', function () { var d2 = ac.createMediaStreamDestination(); var t2 = d2.stream.getAudioTracks()[0]; return [d2.stream.id !== st.id, t2.getSettings().deviceId !== id, t2.label]; });
  await row('constructed node', function () { var d3 = new MediaStreamAudioDestinationNode(ac); var t3 = d3.stream.getAudioTracks()[0]; return [Object.prototype.toString.call(d3.stream), t3.kind, t3.label, t3.getSettings().channelCount]; });
  try { ac.close(); } catch (e) {}
  return out;
})()
