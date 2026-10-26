(() => {
  const out = [];
  const t = (label, f) => {
    try { const v = f(); out.push([label, 'ok', typeof v === 'number' && isNaN(v) ? 'NaN' : (v === undefined ? 'undefined' : (typeof v === 'object' && v !== null ? Object.prototype.toString.call(v) : v))]); }
    catch (e) { out.push([label, e.name, e.message]); }
  };
  // required members, every family the shell constructor builds
  t('blob-empty', () => new BlobEvent('x', {}));
  t('blob-none', () => new BlobEvent('x'));
  t('blob-wrong', () => new BlobEvent('x', { data: 1 }));
  t('blob-data', () => { const b = new Blob(['a']); return new BlobEvent('x', { data: b }).data === b; });
  t('blob-timecode-absent', () => new BlobEvent('x', { data: new Blob([]) }).timecode);
  t('blob-timecode', () => new BlobEvent('x', { data: new Blob([]), timecode: 5 }).timecode);
  t('blob-timecode-nan', () => new BlobEvent('x', { data: new Blob([]), timecode: NaN }));
  t('blob-not-object', () => new BlobEvent('x', 5));
  t('formdata-empty', () => new FormDataEvent('x', {}));
  t('formdata', () => new FormDataEvent('x', { formData: new FormData() }).formData);
  t('rtcerror-empty', () => new RTCError({}));
  t('rtcerror-enum', () => new RTCError({ errorDetail: 'nope' }));
  t('rtcerror-ok', () => new RTCError({ errorDetail: 'data-channel-failure' }).errorDetail);
  t('rtcerrorevent-wrong', () => new RTCErrorEvent('x', { error: {} }));
  t('rtcerrorevent', () => new RTCErrorEvent('x', { error: new RTCError({ errorDetail: 'data-channel-failure' }) }).error);
  t('icee-code', () => new RTCPeerConnectionIceErrorEvent('x', { errorCode: 701 }).errorCode);
  t('icee-defaults', () => { const e = new RTCPeerConnectionIceErrorEvent('x', { errorCode: 7 }); return [e.address, e.port, e.url, e.errorText].join('|'); });
  t('mkm-empty', () => new MediaKeyMessageEvent('x', {}));
  t('mkm-type', () => new MediaKeyMessageEvent('x', { messageType: 'license-request', message: new ArrayBuffer(1) }).messageType);
  t('mkm-buffer-wrong', () => new MediaKeyMessageEvent('x', { messageType: 'license-request', message: 1 }));
  t('tpc', () => new TaskPriorityChangeEvent('x', { previousPriority: 'user-blocking' }).previousPriority);
  t('tpc-enum', () => new TaskPriorityChangeEvent('x', { previousPriority: 'urgent' }));
  t('gamepad-null', () => new GamepadEvent('x').gamepad);
  t('fontfaces', () => { const f = new FontFaceSetLoadEvent('x').fontfaces; return Array.isArray(f) + ':' + f.length; });
  t('count-empty', () => new CountQueuingStrategy({}));
  t('count-hwm', () => new CountQueuingStrategy({ highWaterMark: 3 }).highWaterMark);
  t('audiodata-empty', () => new AudioData({}));
  t('audiodecoder-empty', () => new AudioDecoder({}));
  t('audiodecoder-cb', () => new AudioDecoder({ output: 1, error: () => {} }));
  // the dictionary is read once, in WebIDL's order: inherited EventInit first, then own members sorted
  t('read-order', () => {
    const log = [];
    const d = new Proxy({ data: new Blob([]) }, { get(o, k) { if (typeof k === 'string') log.push(k); return o[k]; } });
    new BlobEvent('x', d);
    return log.join(',');
  });
  t('read-order-throw', () => {
    const log = [];
    const d = new Proxy({}, { get(o, k) { if (typeof k === 'string') log.push(k); return o[k]; } });
    try { new BlobEvent('x', d); } catch (e) { log.push(e.name); }
    return log.join(',');
  });
  // the own getters are the interface's, behind its brand
  t('brand', () => Object.getOwnPropertyDescriptor(BlobEvent.prototype, 'data').get.call(new Event('x')));
  t('event-base', () => { const e = new BlobEvent('y', { data: new Blob([]), bubbles: true }); return e.type + ':' + e.bubbles + ':' + e.cancelable; });
  return out;
})()
