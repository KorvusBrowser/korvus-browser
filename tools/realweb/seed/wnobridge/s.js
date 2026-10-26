importScripts('probe.js');
const r = probe();
self.onconnect = (e) => { e.ports[0].postMessage(r); };
