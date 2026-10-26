// GOAL-4 W3b direction-A fixture: the worker receives a transferred MessagePort as e.ports[0] and relays
// each message it gets back over that same port (proving cross-isolate MessagePort delivery both ways).
self.onmessage = (e) => {
  const port = e.ports && e.ports[0];
  if (!port) { self.postMessage("NO-PORT"); return; }
  port.onmessage = (ev) => { port.postMessage("worker-port-got:" + ev.data); };
};
