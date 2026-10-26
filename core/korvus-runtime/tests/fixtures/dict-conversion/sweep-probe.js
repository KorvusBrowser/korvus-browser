(async () => {
  // Every dictionary argument read through a logging Proxy: which members are read, in which order, and what the
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
  // every Event interface with a constructor: new X('x', dict)
  const events = Object.getOwnPropertyNames(window).filter((n) => /Event$/.test(n) && typeof window[n] === 'function').sort();
  for (const n of events) await t('new ' + n, (d) => new window[n]('x', d));
  const blob = URL.createObjectURL(new Blob(['']));
  const cv = () => document.createElement('canvas');
  const el = () => document.createElement('div');
  const ctx = new OfflineAudioContext(1, 128, 44100);
  const cases = [
    ['new Blob', (d) => new Blob([], d)],
    ['new File', (d) => new File([], 'f', d)],
    ['new TextDecoder', (d) => new TextDecoder('utf-8', d)],
    ['TextDecoder.decode', (d) => new TextDecoder().decode(new Uint8Array(1), d)],
    ['new TextDecoderStream', (d) => new TextDecoderStream('utf-8', d)],
    ['new Request', (d) => new Request('/', d)],
    ['new Response', (d) => new Response('', d)],
    ['Response.json', (d) => Response.json(1, d)],
    ['fetch', (d) => fetch('/nowhere', d)],
    ['new Headers', (d) => new Headers(d)],
    ['new Worker', (d) => new Worker(blob, d)],
    ['new SharedWorker', (d) => new SharedWorker(blob, d)],
    ['new Notification', (d) => new Notification('n', d)],
    ['new IntersectionObserver', (d) => new IntersectionObserver(() => {}, d)],
    ['ResizeObserver.observe', (d) => new ResizeObserver(() => {}).observe(el(), d)],
    ['MutationObserver.observe', (d) => new MutationObserver(() => {}).observe(document, d)],
    ['PerformanceObserver.observe', (d) => new PerformanceObserver(() => {}).observe(d)],
    ['performance.mark', (d) => performance.mark('m', d)],
    ['performance.measure', (d) => performance.measure('m', d)],
    ['new PerformanceMark', (d) => new PerformanceMark('m', d)],
    ['addEventListener', (d) => addEventListener('x', () => {}, d)],
    ['removeEventListener', (d) => removeEventListener('x', () => {}, d)],
    ['getContext 2d', (d) => cv().getContext('2d', d)],
    ['getContext webgl', (d) => cv().getContext('webgl', d)],
    ['getContext webgl2', (d) => cv().getContext('webgl2', d)],
    ['getContext bitmaprenderer', (d) => cv().getContext('bitmaprenderer', d)],
    ['OffscreenCanvas.getContext 2d', (d) => new OffscreenCanvas(1, 1).getContext('2d', d)],
    ['OffscreenCanvas.convertToBlob', (d) => new OffscreenCanvas(1, 1).convertToBlob(d)],
    ['createImageBitmap', (d) => createImageBitmap(new ImageData(1, 1), d)],
    ['new ImageData', (d) => new ImageData(1, 1, d)],
    ['DOMMatrix.fromMatrix', (d) => DOMMatrix.fromMatrix(d)],
    ['DOMMatrixReadOnly.fromMatrix', (d) => DOMMatrixReadOnly.fromMatrix(d)],
    ['DOMPoint.fromPoint', (d) => DOMPoint.fromPoint(d)],
    ['DOMRect.fromRect', (d) => DOMRect.fromRect(d)],
    ['DOMQuad.fromRect', (d) => DOMQuad.fromRect(d)],
    ['DOMQuad.fromQuad', (d) => DOMQuad.fromQuad(d)],
    ['DOMMatrix.transformPoint', (d) => new DOMMatrix().transformPoint(d)],
    ['structuredClone', (d) => structuredClone(1, d)],
    ['postMessage', (d) => postMessage(1, d)],
    ['MessagePort.postMessage', (d) => new MessageChannel().port1.postMessage(1, d)],
    ['attachShadow', (d) => el().attachShadow(d)],
    ['scrollIntoView', (d) => el().scrollIntoView(d)],
    ['Element.scroll', (d) => el().scroll(d)],
    ['Element.scrollTo', (d) => el().scrollTo(d)],
    ['Element.scrollBy', (d) => el().scrollBy(d)],
    ['window.scroll', (d) => scroll(d)],
    ['window.scrollTo', (d) => scrollTo(d)],
    ['window.scrollBy', (d) => scrollBy(d)],
    ['focus', (d) => el().focus(d)],
    ['getRootNode', (d) => el().getRootNode(d)],
    ['createElement', (d) => document.createElement('div', d)],
    ['createElementNS', (d) => document.createElementNS('http://www.w3.org/1999/xhtml', 'div', d)],
    ['customElements.define', (d) => customElements.define('x-sweep-' + out.length, class extends HTMLElement {}, d)],
    ['checkVisibility', (d) => el().checkVisibility(d)],
    ['getHTML', (d) => el().getHTML(d)],
    ['animate', (d) => el().animate([], d)],
    ['getAnimations', (d) => el().getAnimations(d)],
    ['new KeyframeEffect', (d) => new KeyframeEffect(null, [], d)],
    ['new DocumentTimeline', (d) => new DocumentTimeline(d)],
    ['new ScrollTimeline', (d) => new ScrollTimeline(d)],
    ['new ViewTimeline', (d) => new ViewTimeline(d)],
    ['new FontFace', (d) => new FontFace('f', 'url(x)', d)],
    ['CSS.registerProperty', (d) => CSS.registerProperty(d)],
    ['new CountQueuingStrategy', (d) => new CountQueuingStrategy(d)],
    ['new AudioContext', (d) => new AudioContext(d).close()],
    ['new OfflineAudioContext', (d) => new OfflineAudioContext(d)],
    ['new GainNode', (d) => new GainNode(ctx, d)],
    ['new OscillatorNode', (d) => new OscillatorNode(ctx, d)],
    ['new AnalyserNode', (d) => new AnalyserNode(ctx, d)],
    ['new BiquadFilterNode', (d) => new BiquadFilterNode(ctx, d)],
    ['new DynamicsCompressorNode', (d) => new DynamicsCompressorNode(ctx, d)],
    ['new AudioBuffer', (d) => new AudioBuffer(d)],
    ['new AudioBufferSourceNode', (d) => new AudioBufferSourceNode(ctx, d)],
    ['new ConstantSourceNode', (d) => new ConstantSourceNode(ctx, d)],
    ['new PannerNode', (d) => new PannerNode(ctx, d)],
    ['new DelayNode', (d) => new DelayNode(ctx, d)],
    ['new WaveShaperNode', (d) => new WaveShaperNode(ctx, d)],
    ['new StereoPannerNode', (d) => new StereoPannerNode(ctx, d)],
    ['new ConvolverNode', (d) => new ConvolverNode(ctx, d)],
    ['new ChannelMergerNode', (d) => new ChannelMergerNode(ctx, d)],
    ['createPeriodicWave', (d) => ctx.createPeriodicWave(new Float32Array(2), new Float32Array(2), d)],
    ['new RTCPeerConnection', (d) => new RTCPeerConnection(d).close()],
    ['RTCPeerConnection.createOffer', (d) => { const p = new RTCPeerConnection(); const r = p.createOffer(d); p.close(); return r; }],
    ['RTCPeerConnection.createDataChannel', (d) => { const p = new RTCPeerConnection(); try { return p.createDataChannel('c', d); } finally { p.close(); } }],
    ['RTCPeerConnection.addTransceiver', (d) => { const p = new RTCPeerConnection(); try { return p.addTransceiver('audio', d); } finally { p.close(); } }],
    ['new RTCSessionDescription', (d) => new RTCSessionDescription(d)],
    ['new RTCIceCandidate', (d) => new RTCIceCandidate(d)],
    ['crypto.subtle.digest', (d) => crypto.subtle.digest(d, new Uint8Array(1))],
    ['crypto.subtle.generateKey', (d) => crypto.subtle.generateKey(d, false, ['sign'])],
    ['crypto.subtle.importKey', (d) => crypto.subtle.importKey('raw', new Uint8Array(16), d, false, ['encrypt'])],
    ['navigator.permissions.query', (d) => navigator.permissions.query(d)],
    ['navigator.locks.request', (d) => navigator.locks.request('l', d, () => {})],
    ['navigator.locks.query', (d) => navigator.locks.query(d)],
    ['navigator.credentials.get', (d) => navigator.credentials.get(d)],
    ['navigator.serviceWorker.register', (d) => navigator.serviceWorker.register('/sw-none.js', d)],
    ['navigator.canShare', (d) => navigator.canShare(d)],
    ['scheduler.postTask', (d) => scheduler.postTask(() => {}, d)],
    ['scheduler.yield', (d) => scheduler.yield(d)],
    ['new TaskController', (d) => new TaskController(d)],
    ['requestIdleCallback', (d) => requestIdleCallback(() => {}, d)],
    ['cookieStore.get', (d) => cookieStore.get(d)],
    ['cookieStore.getAll', (d) => cookieStore.getAll(d)],
    ['cookieStore.set', (d) => cookieStore.set(d)],
    ['cookieStore.delete', (d) => cookieStore.delete(d)],
    ['caches.match', (d) => caches.match('/', d)],
    ['document.startViewTransition', (d) => { try { return document.startViewTransition(d).skipTransition(); } catch (e) { throw e; } }],
    ['new EventSource', (d) => new EventSource('/es-none', d).close()],
    ['new EncodedVideoChunk', (d) => new EncodedVideoChunk(d)],
    ['new VideoEncoder', (d) => new VideoEncoder(d)],
    ['new VideoDecoder', (d) => new VideoDecoder(d)],
    ['new AudioEncoder', (d) => new AudioEncoder(d)],
    ['new MediaRecorder', (d) => new MediaRecorder(new MediaStream(), d)],
    ['new Path2D addPath', (d) => new Path2D().addPath(new Path2D(), d)],
    ['ctx2d.getImageData', (d) => cv().getContext('2d').getImageData(0, 0, 1, 1, d)],
    ['ctx2d.createImageData', (d) => cv().getContext('2d').createImageData(1, 1, d)],
    ['new URLPattern', (d) => new URLPattern(d)],
    ['document.caretPositionFromPoint', (d) => document.caretPositionFromPoint(0, 0, d)],
    ['CSSStyleSheet ctor', (d) => new CSSStyleSheet(d)],
    ['new URLSearchParams', (d) => new URLSearchParams(d)],
    ['showPopover', (d) => { const e = el(); e.popover = 'manual'; document.body.appendChild(e); try { return e.showPopover(d); } finally { e.remove(); } }],
    ['togglePopover', (d) => { const e = el(); e.popover = 'manual'; document.body.appendChild(e); try { return e.togglePopover(d); } finally { e.remove(); } }],
    ['Element.animate keyframes', (d) => el().animate(d, 1)],
    ['TrustedTypes.createPolicy', (d) => trustedTypes.createPolicy('sweep' + out.length, d)],
  ];
  for (const [label, f] of cases) await t(label, f);
  // IndexedDB's dictionaries are read inside a versionchange transaction
  const idb = [];
  await new Promise((res) => {
    const r = indexedDB.open('dict-sweep-' + Date.now());
    r.onupgradeneeded = () => {
      const db = r.result;
      for (const [label, f] of [
        ['IDBDatabase.createObjectStore', (d) => db.createObjectStore('s', d)],
        ['IDBObjectStore.createIndex', (d) => r.transaction.objectStore('s').createIndex('i', 'k', d)],
      ]) {
        const reads = [];
        let outcome;
        try { f(spy(reads)); outcome = 'ok'; } catch (e) { outcome = (e && e.name) + ': ' + (e && e.message); }
        idb.push([label, reads.join(','), outcome]);
      }
    };
    r.onsuccess = () => {
      const db = r.result;
      const reads = [];
      let outcome;
      try { db.transaction('s', 'readonly', spy(reads)); outcome = 'ok'; } catch (e) { outcome = (e && e.name) + ': ' + (e && e.message); }
      idb.push(['IDBDatabase.transaction', reads.join(','), outcome]);
      db.close();
      res();
    };
    r.onerror = () => res();
  });
  out.push(...idb);
  return out;
})()
