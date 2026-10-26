// TEMPLATES P6 B22: a service worker's `clients` is a Clients, each client a WindowClient : Client, every
// member on the prototypes (they were plain objects with own functions, and the interfaces were absent).
const settle = (p) => Promise.resolve(p).then((v) => 'resolved:' + (v === undefined ? 'undefined' : v === null ? 'null' : Array.isArray(v) ? 'array' + v.length : Object.prototype.toString.call(v)), (e) => 'rejected:' + e.constructor.name + ':' + e.name + ':' + e.message);
self.addEventListener('message', async (e) => {
  const out = {};
  const t = (k, f) => { try { const v = f(); out[k] = v === undefined ? 'undefined' : v === null ? 'null' : typeof v === 'object' ? Object.prototype.toString.call(v) : String(v); } catch (x) { out[k] = 'ERR ' + x.constructor.name + ': ' + x.message; } };
  for (const n of ['Clients', 'Client', 'WindowClient']) {
    const I = self[n];
    t(n, () => typeof I === 'function' ? [I.length, Reflect.ownKeys(I.prototype).map(String).join(), Object.getPrototypeOf(I.prototype) === Object.prototype ? 'OP' : Object.getPrototypeOf(I).name].join(' | ') : typeof I);
    t(n + '.new', () => new I());
  }
  const c = self.clients;
  t('clientsTag', () => c); t('clientsOwn', () => Reflect.ownKeys(c).join()); t('same', () => self.clients === c);
  const list = await c.matchAll({ includeUncontrolled: true, type: 'window' });
  t('frozen', () => Object.isFrozen(list));
  const cl = list[0];
  t('n', () => list.length);
  t('clientTag', () => cl); t('clientOwn', () => Reflect.ownKeys(cl).join());
  t('url', () => cl.url.replace(/:\d+\//, ':PORT/'));
  t('id', () => typeof cl.id + ':' + (cl.id.length > 0));
  t('type', () => cl.type + '|' + cl.frameType + '|' + cl.visibilityState + '|' + cl.focused);
  out.focus = await settle(cl.focus());
  out.navigateBlank = await settle(cl.navigate('about:blank'));
  out.navigate = await settle(cl.navigate('/x'));
  out.navigateNoArg = await settle(cl.navigate());
  const again = await c.matchAll({ includeUncontrolled: true });
  t('newPerCall', () => again[0] !== cl && again[0].id === cl.id);
  out.getSame = await settle(c.get(cl.id));
  out.getUnknown = await settle(c.get('nope'));
  out.getNoArg = await settle(c.get());
  out.workerType = await settle(c.matchAll({ type: 'worker' }));
  out.badType = await settle(c.matchAll({ type: 'bogus' }));
  out.openWindow = await settle(c.openWindow('/x'));
  out.claim = await settle(c.claim());
  out.matchAllForeign = await settle(Clients.prototype.matchAll.call({}));
  t('postMessage', () => cl.postMessage('x'));
  t('pmNoArg', () => cl.postMessage());
  t('pmForeign', () => Client.prototype.postMessage.call({}, 1));
  t('urlForeign', () => Object.getOwnPropertyDescriptor(Client.prototype, 'url').get.call({}));
  t('fakeClient', () => Object.getOwnPropertyDescriptor(Client.prototype, 'url').get.call(Object.create(WindowClient.prototype)));
  out.lens = [Clients.prototype.get.length, Clients.prototype.matchAll.length, Clients.prototype.openWindow.length, Clients.prototype.claim.length, Client.prototype.postMessage.length, WindowClient.prototype.focus.length, WindowClient.prototype.navigate.length].join();
  e.source.postMessage(JSON.stringify(out));
});
