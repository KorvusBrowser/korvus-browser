importScripts('probe.js');
const settle = (p) => Promise.resolve(p).then(() => 'resolved', (e) => 'rejected:' + e.constructor.name + ':' + e.message);
self.addEventListener('message', async (e) => {
  const r = probe(['clients', 'registration', 'oninstall', 'onactivate', 'onfetch', 'onmessage'], []);
  const fake = Object.create(Object.getPrototypeOf(self));
  const skip = [await settle(self.skipWaiting.call({})), await settle(self.skipWaiting.call(fake)), await settle(self.skipWaiting())].join(' | ');
  e.source.postMessage(r + ' || skipWaiting ' + skip);
});
