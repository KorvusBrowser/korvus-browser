importScripts('probe.js');
const r = probe(['name', 'onconnect'], []);
self.onconnect = (e) => { e.ports[0].postMessage(r); };
