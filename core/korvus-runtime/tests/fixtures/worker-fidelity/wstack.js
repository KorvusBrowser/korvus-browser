function f() { return new Error('x').stack }
postMessage('net: ' + JSON.stringify(f()));
