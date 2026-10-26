importScripts('probe.js');
probeIter().then((r) => postMessage(r));
