(async () => {
  // MediaCapabilities.decodingInfo / encodingInfo: the members read (a logging proxy, nested for video/audio), in
  // order, and what each call answers -- a throw, a rejection, or the resolved info. Rows: [label, reads, outcome].
  const out = [];
  const name = (k) => (typeof k === 'symbol' ? '@' + k.description : String(k));
  const spy = (reads, prefix, values) => new Proxy({}, {
    get(o, k) {
      const n = name(k);
      reads.push(prefix + n);
      const v = values[n];
      if (v && typeof v === 'object' && v.__nested) return spy(reads, n + '.', v.values);
      return v;
    },
    has(o, k) { reads.push('has:' + prefix + name(k)); return name(k) in values; },
  });
  const settle = async (f) => {
    let p;
    try { p = f(); } catch (e) { return 'threw ' + e.name + ': ' + e.message; }
    if (!p || typeof p.then !== 'function') return 'returned ' + String(p);
    try {
      const r = await p;
      return 'resolved ' + JSON.stringify([r.supported, r.smooth, r.powerEfficient, r.keySystemAccess === null ? null : typeof r.keySystemAccess, Object.keys(r).join()]);
    } catch (e) { return 'rejected ' + e.name + ': ' + e.message; }
  };
  const mc = navigator.mediaCapabilities;
  const video = (extra) => ({__nested: true, values: Object.assign({contentType: 'video/webm; codecs="vp9"', width: 640, height: 480, bitrate: 1000, framerate: 30}, extra || {})});
  const audio = (extra) => ({__nested: true, values: Object.assign({contentType: 'audio/webm; codecs="opus"'}, extra || {})});
  const t = async (label, method, values) => {
    const reads = [];
    const outcome = await settle(() => mc[method](spy(reads, '', values)));
    out.push([label, reads.join(','), outcome]);
  };
  await t('dec/video', 'decodingInfo', {type: 'file', video: video()});
  await t('dec/audio', 'decodingInfo', {type: 'file', audio: audio()});
  await t('dec/both', 'decodingInfo', {type: 'media-source', video: video(), audio: audio()});
  await t('dec/none', 'decodingInfo', {type: 'file'});
  await t('dec/notype', 'decodingInfo', {video: video()});
  await t('dec/badtype', 'decodingInfo', {type: 'x', video: video()});
  await t('dec/webrtc', 'decodingInfo', {type: 'webrtc', video: video({contentType: 'video/VP8'})});
  await t('dec/video-nocontent', 'decodingInfo', {type: 'file', video: {__nested: true, values: {width: 1, height: 1, bitrate: 1, framerate: 1}}});
  await t('dec/video-nowidth', 'decodingInfo', {type: 'file', video: video({width: undefined})});
  await t('dec/width-nan', 'decodingInfo', {type: 'file', video: video({width: NaN})});
  await t('dec/width-neg', 'decodingInfo', {type: 'file', video: video({width: -1})});
  await t('dec/framerate-nan', 'decodingInfo', {type: 'file', video: video({framerate: NaN})});
  await t('dec/framerate-0', 'decodingInfo', {type: 'file', video: video({framerate: 0})});
  await t('dec/bitrate-str', 'decodingInfo', {type: 'file', video: video({bitrate: '7'})});
  await t('dec/hdr-bad', 'decodingInfo', {type: 'file', video: video({hdrMetadataType: 'x'})});
  await t('dec/hdr-ok', 'decodingInfo', {type: 'file', video: video({hdrMetadataType: 'smpteSt2086', colorGamut: 'p3', transferFunction: 'pq'})});
  await t('dec/content-bad', 'decodingInfo', {type: 'file', video: video({contentType: 'nonsense'})});
  await t('dec/content-audio-in-video', 'decodingInfo', {type: 'file', video: video({contentType: 'audio/webm; codecs="opus"'})});
  await t('dec/content-two-codecs', 'decodingInfo', {type: 'file', video: video({contentType: 'video/mp4; codecs="avc1.42E01E, mp4a.40.2"'})});
  await t('dec/audio-channels', 'decodingInfo', {type: 'file', audio: audio({channels: '2', bitrate: 128000, samplerate: 48000, spatialRendering: true})});
  await t('dec/keysystem', 'decodingInfo', {type: 'file', video: video(), keySystemConfiguration: {__nested: true, values: {keySystem: 'org.w3.clearkey'}}});
  await t('enc/record', 'encodingInfo', {type: 'record', video: video()});
  await t('enc/webrtc', 'encodingInfo', {type: 'webrtc', video: video({contentType: 'video/VP8'})});
  await t('enc/badtype', 'encodingInfo', {type: 'file', video: video()});
  await t('enc/none', 'encodingInfo', {type: 'webrtc'});
  // plain arguments
  for (const [label, method, arg] of [['dec/undefined', 'decodingInfo', undefined], ['dec/null', 'decodingInfo', null], ['dec/5', 'decodingInfo', 5], ['dec/empty', 'decodingInfo', {}], ['enc/empty', 'encodingInfo', {}]]) {
    out.push([label, '', await settle(() => mc[method](arg))]);
  }
  out.push(['dec/0args', '', await settle(() => mc.decodingInfo())]);
  // and the `webrtc` answers: the device's RTP codecs, an encoder's scalability modes, a decoder's spatial layers
  const run = async (method, cfg) => { try { const r = await mc[method](cfg); return [r.supported, r.smooth, r.powerEfficient].join(); } catch (e) { return e.name + ': ' + e.message; } };
  const v = (ct, extra) => Object.assign({contentType: ct, width: 640, height: 480, bitrate: 1000, framerate: 30}, extra || {});
  const vids = ['video/VP8', 'video/vp8', 'video/VP9', 'video/H264', 'video/AV1', 'video/H265', 'video/foo', 'video/rtx', 'video/red', 'video/ulpfec',
    'video/VP8; x=1', 'video/H264;profile-level-id=42e01f', 'video/H264;profile-level-id=640c1f;packetization-mode=1', 'video/VP9;profile-id=2', 'audio/opus', 'nonsense'];
  for (const ct of vids) {
    out.push(['webrtc/dec ' + ct, '', await run('decodingInfo', {type: 'webrtc', video: v(ct)})]);
    out.push(['webrtc/enc ' + ct, '', await run('encodingInfo', {type: 'webrtc', video: v(ct)})]);
  }
  for (const ct of ['audio/opus', 'audio/OPUS', 'audio/PCMU', 'audio/PCMA', 'audio/G722', 'audio/ISAC', 'audio/telephone-event', 'audio/CN', 'audio/red', 'audio/foo', 'audio/opus; x=1', 'video/VP8']) {
    out.push(['webrtc/dec ' + ct, '', await run('decodingInfo', {type: 'webrtc', audio: {contentType: ct}})]);
    out.push(['webrtc/enc ' + ct, '', await run('encodingInfo', {type: 'webrtc', audio: {contentType: ct}})]);
  }
  out.push(['webrtc/enc scal L1T2', '', await run('encodingInfo', {type: 'webrtc', video: v('video/VP8', {scalabilityMode: 'L1T2'})})]);
  out.push(['webrtc/enc scal L1T3 VP9', '', await run('encodingInfo', {type: 'webrtc', video: v('video/VP9', {scalabilityMode: 'L3T3'})})]);
  out.push(['webrtc/enc scal bogus', '', await run('encodingInfo', {type: 'webrtc', video: v('video/VP8', {scalabilityMode: 'X9'})})]);
  out.push(['webrtc/dec scal', '', await run('decodingInfo', {type: 'webrtc', video: v('video/VP8', {scalabilityMode: 'L1T2'})})]);
  out.push(['webrtc/dec spatial', '', await run('decodingInfo', {type: 'webrtc', video: v('video/VP9', {spatialScalability: true})})]);
  out.push(['webrtc/enc spatial', '', await run('encodingInfo', {type: 'webrtc', video: v('video/VP9', {spatialScalability: true})})]);
  out.push(['webrtc/file scal', '', await run('decodingInfo', {type: 'file', video: v('video/webm; codecs="vp8"', {scalabilityMode: 'L1T2'})})]);
  // H.264's profiles and packetization modes (a decoder takes a subset of its formats' profiles, an encoder the first
  // format of the name), VP9's and AV1's profiles, and smooth's edges
  for (const id of ['42001f', '42e01f', '4d001f', '64001f', '640c1f', '58001f', 'f4001f', '6e001f', '42e00b', '640034', '64003c', 'zzzzzz', '4200']) {
    for (const pm of ['0', '1', '2']) {
      const ct = 'video/H264;profile-level-id=' + id + ';packetization-mode=' + pm;
      out.push(['webrtc/h264 ' + id + ' pm' + pm, await run('decodingInfo', {type: 'webrtc', video: v(ct)}), await run('encodingInfo', {type: 'webrtc', video: v(ct)})]);
    }
  }
  for (const ct of ['video/VP9;profile-id=1', 'video/VP9;profile-id=3', 'video/VP9;profile-id=4', 'video/VP9;profile-id=x', 'video/AV1;profile=1', 'video/AV1;profile=2', 'video/AV1;profile=3']) {
    out.push(['webrtc/' + ct, await run('decodingInfo', {type: 'webrtc', video: v(ct)}), await run('encodingInfo', {type: 'webrtc', video: v(ct)})]);
  }
  const edge = (extra) => Object.assign(v('video/VP8'), extra);
  for (const [label, extra] of [['fps 0.4', {framerate: 0.4}], ['fps 1000.4', {framerate: 1000.4}], ['fps 1000.6', {framerate: 1000.6}], ['width 0', {width: 0}],
    ['40000x40000', {width: 40000, height: 40000}], ['7680x4320', {width: 7680, height: 4320}], ['16000x9000', {width: 16000, height: 9000}]]) {
    out.push(['webrtc/smooth ' + label, await run('decodingInfo', {type: 'webrtc', video: edge(extra)}), await run('encodingInfo', {type: 'webrtc', video: edge(extra)})]);
  }
  out.push(['webrtc/opus+vp8', await run('decodingInfo', {type: 'webrtc', audio: {contentType: 'audio/opus'}, video: v('video/VP8')}), await run('encodingInfo', {type: 'webrtc', audio: {contentType: 'audio/opus'}, video: v('video/VP8')})]);
  out.push(['webrtc/isac+vp8', await run('decodingInfo', {type: 'webrtc', audio: {contentType: 'audio/ISAC'}, video: v('video/VP8')}), await run('encodingInfo', {type: 'webrtc', audio: {contentType: 'audio/ISAC'}, video: v('video/VP8')})]);
  out.push(['webrtc/opus+h265', await run('decodingInfo', {type: 'webrtc', audio: {contentType: 'audio/opus'}, video: v('video/H265')}), await run('encodingInfo', {type: 'webrtc', audio: {contentType: 'audio/opus'}, video: v('video/H265')})]);
  out.push(['webrtc/vp8 spatial false', await run('decodingInfo', {type: 'webrtc', video: edge({spatialScalability: false})}), '']);
  out.push(['webrtc/vp8 spatial true', await run('decodingInfo', {type: 'webrtc', video: edge({spatialScalability: true})}), '']);
  out.push(['webrtc/h264 L1T3 L2T1', await run('encodingInfo', {type: 'webrtc', video: v('video/H264', {scalabilityMode: 'L1T3'})}), await run('encodingInfo', {type: 'webrtc', video: v('video/H264', {scalabilityMode: 'L2T1'})})]);
  out.push(['webrtc/av1 S3T3 L3T3_KEY', await run('encodingInfo', {type: 'webrtc', video: v('video/AV1', {scalabilityMode: 'S3T3'})}), await run('encodingInfo', {type: 'webrtc', video: v('video/AV1', {scalabilityMode: 'L3T3_KEY'})})]);
  return out;
})()
