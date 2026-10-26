(async () => {
  // navigator.sendBeacon's answers and errors for URL and body forms, what it reads of the page, and the timeline
  // entries it leaves (Blink's NavigatorBeacon / PingLoader).
  const R = [];
  const reads = [];
  const spy = (proto, name, label) => {
    const d = Object.getOwnPropertyDescriptor(proto, name);
    if (!d) return () => {};
    Object.defineProperty(proto, name, { configurable: true, get() { reads.push(label); return d.get.call(this); } });
    return () => Object.defineProperty(proto, name, d);
  };
  const undo = [spy(Node.prototype, 'baseURI', 'baseURI'), spy(Document.prototype, 'URL', 'URL'), spy(Blob.prototype, 'type', 'Blob.type'), spy(Blob.prototype, 'size', 'Blob.size'), spy(URL.prototype, 'href', 'URL.href')];
  const b = (label, ...args) => {
    let r;
    try { r = navigator.sendBeacon(...args); } catch (e) { r = 'throw:' + e.name + ': ' + e.message; }
    R.push([label, r, reads.splice(0).join(',')]);
  };
  b('relative string', '/b1', 'x');
  b('no data', '/b2');
  b('null data', '/b3', null);
  b('undefined data', '/b4', undefined);
  b('blob safelisted', '/b5', new Blob(['x'], { type: 'text/plain;charset=utf-8' }));
  b('blob json', '/b6', new Blob(['{}'], { type: 'application/json' }));
  b('form data', '/b7', (() => { const f = new FormData(); f.append('a', '1'); return f; })());
  b('url params', '/b8', new URLSearchParams('a=1'));
  b('array buffer', '/b9', new Uint8Array([1, 2, 3]).buffer);
  b('typed array', '/b10', new Uint8Array([1, 2, 3]));
  b('object to string', '/b11', { toString() { reads.push('toString'); return 'obj'; } });
  b('number', '/b12', 42);
  b('stream', '/b13', new ReadableStream());
  b('too big', '/b14', 'x'.repeat(70000));
  b('just under quota', '/b15', 'x'.repeat(60000));
  b('ill-formed url', 'http://[x', 'x');
  b('data url', 'data:text/plain,x', 'x');
  b('javascript url', 'javascript:void 0', 'x');
  b('ftp url', 'ftp://example.invalid/', 'x');
  b('no args');
  undo.forEach((u) => u());
  await new Promise((r) => setTimeout(r, 800));
  const es = performance.getEntriesByType('resource').filter((e) => /\/b\d+$/.test(e.name));
  for (const e of es) R.push(['entry ' + e.name.replace(/^.*\//, ''), e.initiatorType, e.responseStatus, e.renderBlockingStatus, e.nextHopProtocol]);
  return R;
})()
