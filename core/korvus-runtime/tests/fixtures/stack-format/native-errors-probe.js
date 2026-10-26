(function () {
  // The errors the platform throws and rejects with: what `message`, the stack's head line and the error's own
  // properties read. Blink composes an exception's operation context ("Failed to execute 'x' on 'Y': ") AFTER the
  // error was made (V8's exception-propagation callback, `ApplyContextToException`): a DOMException's message takes
  // it; an ECMAScript error's `message` is redefined with it (CreateDataProperty -- enumerable) while V8 formats
  // `stack` from the message the error was made with. Rows in a fixed order, whatever order the rejections settle in.
  function d(e, k) { var x = Object.getOwnPropertyDescriptor(e, k); return x ? [('value' in x) ? 'data' : 'accessor', x.enumerable, x.configurable, !!x.writable] : null; }
  function info(e) {
    return [Object.prototype.toString.call(e), e.name, e.message, String(e.stack).split('\n')[0], Object.keys(e).join(','),
      Object.getOwnPropertyNames(e).join(','), d(e, 'message'), d(e, 'stack'), String(e), e instanceof TypeError, e.constructor === TypeError];
  }
  function thrown(f) { try { f(); return ['no throw']; } catch (e) { return info(e); } }
  function rej(f) {
    var p;
    try { p = f(); } catch (e) { return Promise.resolve(['threw'].concat(info(e))); }
    return Promise.resolve(p).then(function () { return ['resolved']; }, function (e) { return info(e); });
  }
  var ctx = document.createElement('canvas').getContext('2d');
  var sync = [
    ['appendChild(5)', function () { document.body.appendChild(5); }],
    ['new Event()', function () { new Event(); }],
    ['Event("x")', function () { Event('x'); }],
    ['new URL("nope")', function () { new URL('nope'); }],
    ['new Blob(5)', function () { new Blob(5); }],
    ['querySelector()', function () { document.querySelector(); }],
    ['setAttribute()', function () { document.body.setAttribute(); }],
    ['roundRect radius -1', function () { ctx.roundRect(0, 0, 10, 10, -1); }],
    ['arc radius -1', function () { ctx.arc(0, 0, -1, 0, 1); }],
    ['createElement("1")', function () { document.createElement('1'); }],
    ['structuredClone(fn)', function () { structuredClone(function () {}); }],
    ['illegal invocation', function () { Node.prototype.appendChild.call({}, document.body); }],
    ['addEventListener()', function () { document.addEventListener(); }],
    ['localStorage.setItem()', function () { localStorage.setItem(); }],
    ['new Uint8Array(-1)', function () { new Uint8Array(-1); }],
    ['JSON.parse("{")', function () { JSON.parse('{'); }],
    ['getComputedStyle(5)', function () { getComputedStyle(5); }],
    ['new MutationObserver(5)', function () { new MutationObserver(5); }],
    ['postMessage()', function () { postMessage(); }],
    ['new Request("http://[")', function () { new Request('http://['); }],
    ['new Response(null, {status: 1})', function () { new Response(null, { status: 1 }); }],
    ['TextEncoder encodeInto()', function () { new TextEncoder().encodeInto(); }],
    ['crypto.getRandomValues(Float32Array)', function () { crypto.getRandomValues(new Float32Array(1)); }]
  ];
  var async = [
    ['fetch("http://[")', function () { return fetch('http://['); }],
    ['subtle.digest("nope")', function () { return crypto.subtle.digest('nope', new Uint8Array(1)); }],
    ['Response("").json()', function () { return new Response('').json(); }],
    ['Response("{").json()', function () { return new Response('{').json(); }],
    ['permissions.query({name: "nope"})', function () { return navigator.permissions.query({ name: 'nope' }); }],
    ['permissions.query()', function () { return navigator.permissions.query(); }],
    ['Clipboard readText illegal invocation', function () { return Clipboard.prototype.readText.call({}); }],
    ['locked ReadableStream cancel', function () { var rs = new ReadableStream(); rs.getReader(); return rs.cancel(); }],
    ['locked WritableStream abort', function () { var ws = new WritableStream(); ws.getWriter(); return ws.abort(); }],
    ['locks.request("a", 5)', function () { return navigator.locks.request('a', 5); }],
    ['pipeTo(5)', function () { return new Response('x').body.pipeTo(5); }],
    ['body already used', function () { var r = new Response('x'); r.text(); return r.text(); }],
    ['fetch()', function () { return fetch(); }],
    ['scheduler.postTask(5)', function () { return scheduler.postTask(5); }]
  ];
  var rows = sync.map(function (c) { return [c[0]].concat(thrown(c[1])); });
  return Promise.all(async.map(function (c) { return rej(c[1]); })).then(function (r) {
    return rows.concat(r.map(function (x, i) { return [async[i][0]].concat(x); }));
  });
})()
