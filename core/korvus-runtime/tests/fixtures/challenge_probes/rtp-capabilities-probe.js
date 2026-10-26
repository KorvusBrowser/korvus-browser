(() => {
  const out = [];
  for (const C of ['RTCRtpReceiver', 'RTCRtpSender']) for (const k of ['audio', 'video']) {
    const c = window[C].getCapabilities(k);
    out.push([C + '.' + k, C === 'RTCRtpReceiver' && k === 'video' ? 'device' : JSON.stringify(c), Object.keys(c).join(','), c.codecs.map(x => Object.keys(x).join(',')).filter((v, i, a) => a.indexOf(v) === i)]);
  }
  out.push(['proto', Object.prototype.toString.call(RTCRtpReceiver.getCapabilities('audio')), Object.getPrototypeOf(RTCRtpReceiver.getCapabilities('audio')) === Object.prototype, RTCRtpReceiver.getCapabilities('audio') === RTCRtpReceiver.getCapabilities('audio')]);
  for (const bad of ['', 'Audio', 'application', undefined]) { try { out.push(['bad ' + bad, JSON.stringify(RTCRtpReceiver.getCapabilities(bad))]); } catch (e) { out.push(['bad ' + bad, 'THROW ' + e.name + ': ' + e.message]); } }
  try { out.push(['noarg', RTCRtpReceiver.getCapabilities()]); } catch (e) { out.push(['noarg', 'THROW ' + e.name + ': ' + e.message]); }
  out.push(['len', RTCRtpReceiver.getCapabilities.length, RTCRtpSender.getCapabilities.length]);
  return out;
})()
