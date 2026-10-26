// GOAL-4 W4 SharedWorker fixture: a module-level connection counter proves that repeated `new SharedWorker`
// with the same name share ONE isolate (the counter is shared). Each connection relays messages on its port,
// tagged with its connection number.
let connections = 0;
self.onconnect = (e) => {
  connections++;
  const n = connections;
  const port = e.ports[0];
  port.onmessage = (ev) => { port.postMessage("conn" + n + ":" + ev.data); };
};
