(async () => {
  // More dictionary arguments read through a logging Proxy -- the ones the first sweep did not reach: which members are read, in which order, and what the
  // call answers. Rows: [label, the reads, the outcome]. Left out, as korvus's named residue: the streams (no
  // `pull` source, queuing strategy or backpressure in its model, so no reads of them to compare), `VideoFrame`'s
  // source-state check, and `VideoEncoder.isConfigSupported` (a stub).
  const out = [];
  // a fresh logging Proxy per call, so a read made after its row is over is lost to that row and never lands in another
  const spy = (reads) => {
    const name = (k) => (typeof k === 'symbol' ? '@' + k.description : String(k));
    return new Proxy({}, {
      get(o, k) { reads.push(name(k)); return undefined; },
      has(o, k) { reads.push('has:' + name(k)); return false; },
      ownKeys(o) { reads.push('ownKeys'); return []; },
      getOwnPropertyDescriptor(o, k) { reads.push('gopd:' + name(k)); return undefined; },
    });
  };
  // a promise's answer only when it is already rejected as the call returns (a conversion error); a pending or
  // resolved promise answers 'ok' -- when the browser settles it is no part of the conversion
  const settled = (p) => new Promise((res) => {
    let done = false;
    const end = (v) => { if (!done) { done = true; res(v); } };
    p.then(() => end('ok'), (e) => end('rejected ' + (e && e.name) + ': ' + (e && e.message)));
    Promise.resolve().then(() => 0).then(() => 0).then(() => end('ok'));
  });
  const t = async (label, f) => {
    const reads = [];
    let outcome;
    try {
      const r = f(spy(reads));
      outcome = r && typeof r.then === 'function' ? await settled(r) : 'ok';
    } catch (e) { outcome = (e && e.name) + ': ' + (e && e.message); }
    out.push([label, reads.join(','), outcome]);
  };
  const el = () => document.createElement('div');
  const total = {label: 'x', amount: {currency: 'USD', value: '1'}};
  const cases = [
    ['new MediaMetadata', (d) => new MediaMetadata(d)],
    ['new StaticRange', (d) => new StaticRange(d)],
    ['TaskSignal.any', (d) => TaskSignal.any([], d)],
    ['navigation.updateCurrentEntry', (d) => navigation.updateCurrentEntry(d)],
    ['navigation.navigate', (d) => navigation.navigate('#x', d)],
    ['navigation.traverseTo', (d) => navigation.traverseTo('nope', d)],
    ['requestMediaKeySystemAccess', (d) => navigator.requestMediaKeySystemAccess('org.w3.clearkey', [d])],
    ['getUserMedia', (d) => navigator.mediaDevices.getUserMedia(d)],
    ['getDisplayMedia', (d) => navigator.mediaDevices.getDisplayMedia(d)],
    ['requestMIDIAccess', (d) => navigator.requestMIDIAccess(d)],
    ['geolocation.getCurrentPosition', (d) => navigator.geolocation.getCurrentPosition(() => {}, () => {}, d)],
    ['geolocation.watchPosition', (d) => navigator.geolocation.clearWatch(navigator.geolocation.watchPosition(() => {}, () => {}, d))],
    ['credentials.create', (d) => navigator.credentials.create(d)],
    ['credentials.preventSilentAccess', (d) => navigator.credentials.preventSilentAccess(d)],
    ['new PaymentRequest methods', (d) => new PaymentRequest([d], {total})],
    ['new PaymentRequest details', (d) => new PaymentRequest([{supportedMethods: 'https://a.test/pay'}], d)],
    ['new PaymentRequest options', (d) => new PaymentRequest([{supportedMethods: 'https://a.test/pay'}], {total}, d)],
    ['requestFullscreen', (d) => el().requestFullscreen(d)],
    ['requestPointerLock', (d) => document.body.requestPointerLock(d)],
    ['new ClipboardItem', (d) => new ClipboardItem({'text/plain': 'x'}, d)],
    ['showOpenFilePicker', (d) => showOpenFilePicker(d)],
    ['showSaveFilePicker', (d) => showSaveFilePicker(d)],
    ['showDirectoryPicker', (d) => showDirectoryPicker(d)],
    ['queryLocalFonts', (d) => queryLocalFonts(d)],
    ['navigator.share', (d) => navigator.share(d)],
    ['navigator.hid.requestDevice', (d) => navigator.hid.requestDevice(d)],
    ['navigator.usb.requestDevice', (d) => navigator.usb.requestDevice(d)],
    ['navigator.serial.requestPort', (d) => navigator.serial.requestPort(d)],
    ['navigator.bluetooth.requestDevice', (d) => navigator.bluetooth.requestDevice(d)],
    ['new ReportingObserver', (d) => new ReportingObserver(() => {}, d)],
    ['setHTMLUnsafe', (d) => el().setHTMLUnsafe('<b>x</b>', d)],
    ['Document.parseHTMLUnsafe', (d) => Document.parseHTMLUnsafe('<b>x</b>', d)],
    ['new WebSocketStream', (d) => new WebSocketStream('wss://a.test/', d)],
    ['new WebTransport', (d) => new WebTransport('https://a.test/', d)],
    ['new MediaStreamTrackProcessor', (d) => new MediaStreamTrackProcessor(d)],
    ['new AudioData', (d) => new AudioData(d)],
    ['new EncodedAudioChunk', (d) => new EncodedAudioChunk(d)],
    ['new ImageDecoder', (d) => new ImageDecoder(d)],
    ['new AudioDecoder', (d) => new AudioDecoder(d)],
    ['new VideoColorSpace', (d) => new VideoColorSpace(d)],
    ['new GPUPipelineError', (d) => new GPUPipelineError('m', d)],
    ['CSS.registerProperty', (d) => CSS.registerProperty(d)],
    ['document.requestStorageAccess', (d) => document.requestStorageAccess(d)],
    ['navigator.getBattery', () => navigator.getBattery()],
    ['Element.requestFullscreen navigationUI', (d) => document.documentElement.requestFullscreen(d)],
    ['HTMLMediaElement.setSinkId', () => document.createElement('audio').setSinkId('')],
    ['window.getScreenDetails', () => getScreenDetails()],
    ['new IdleDetector', () => new IdleDetector()],
    ['IdleDetector.start', (d) => new IdleDetector().start(d)],
    ['navigator.wakeLock.request', () => navigator.wakeLock.request('screen')],
    ['new PresentationRequest', () => new PresentationRequest('https://a.test/')],
    ['navigator.storage.getDirectory', () => navigator.storage.getDirectory()],
    ['new BroadcastChannel', () => new BroadcastChannel('b')],
    ['caches.open', () => caches.open('c')],
    ['indexedDB.databases', () => indexedDB.databases()],
    ['new CompressionStream', () => new CompressionStream('gzip')],
    ['navigator.getGamepads', () => navigator.getGamepads()],
  ];
  for (const [label, f] of cases) await t(label, f);
  return out;
})()
