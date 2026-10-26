self.onerror = (m, f, l, c, e) => { postMessage('self.onerror: ' + JSON.stringify([m, f, l, c, e && e.message])); };
self.addEventListener('error', e => postMessage('listener: ' + JSON.stringify([e.message, e.filename, e.lineno, e.colno, e.error && e.error.message])));
try { eval('null.x') } catch (x) { postMessage('eval stack: ' + JSON.stringify(x.stack)) }
try { new Function('a', 'return a.b.c')(1) } catch (x) { postMessage('fn stack: ' + JSON.stringify(x.stack)) }
self.addEventListener('unhandledrejection', e => postMessage('unhandledrejection: ' + String(e.reason && e.reason.message)));
Promise.reject(new Error('rej'));
setTimeout(() => { throw new Error('later') }, 50);
setTimeout(() => { undefinedFn() }, 100);
