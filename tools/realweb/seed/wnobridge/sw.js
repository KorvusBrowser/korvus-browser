importScripts('probe.js');
const r = probe();
self.addEventListener('message', (e) => { e.source.postMessage(r); });
