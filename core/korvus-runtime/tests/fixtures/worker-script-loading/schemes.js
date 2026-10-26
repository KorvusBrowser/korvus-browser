(function () {
  // F3: which URLs a worker may be built from and import, and what each refusal says -- importScripts() in a blob, a
  // URL-loaded and a shared worker (and a module worker), new Worker / new SharedWorker from the page and from a worker.
  var O = location.origin, P = location.port, LH = 'http://localhost:' + P;
  function js(body, opt) {
    var q = '?b=' + encodeURIComponent(body);
    for (var k in (opt || {})) q += '&' + k + '=' + encodeURIComponent(opt[k]);
    return '/js' + q;
  }
  function norm(s) {
    return String(s).replace(/blob:[^ ')"]+/g, 'BLOB').replace(/:\d{2,5}\//g, ':PORT/');
  }
  var pageBlob = URL.createObjectURL(new Blob(['self.L = "page-blob";'], { type: 'text/javascript' }));
  var revoked = URL.createObjectURL(new Blob(['self.L = "revoked";'], { type: 'text/javascript' }));
  URL.revokeObjectURL(revoked);

  // the importScripts cases, run INSIDE a worker (CFG carries the page's origin and blob URLs)
  function workerMain(CFG) {
    var O = CFG.O, LH = CFG.LH, P = CFG.P;
    function js(body, opt) {
      var q = '?b=' + encodeURIComponent(body);
      for (var k in (opt || {})) q += '&' + k + '=' + encodeURIComponent(opt[k]);
      return '/js' + q;
    }
    function norm(s) { return String(s).replace(/blob:[^ ')"]+/g, 'BLOB').replace(/:\d{2,5}\//g, ':PORT/'); }
    function err(e) {
      return 'threw ' + (e && e.name) + ' | ' + (typeof DOMException !== 'undefined' && e instanceof DOMException ? 'DOMException ' + e.code : (e && e.constructor && e.constructor.name)) + ' | ' + norm(e && e.message);
    }
    function imp(label, urls) {
      self.L = 'unset';
      try { importScripts.apply(self, urls); return [label, 'ok ' + norm(self.L)]; } catch (e) { return [label, err(e)]; }
    }
    var rows = [];
    rows.push(imp('no arguments', []));
    rows.push(imp('the page\'s blob', [CFG.pageBlob]));
    var own = URL.createObjectURL(new Blob(['self.L = "own-blob";'], { type: 'text/plain' }));
    rows.push(imp('its own blob (text/plain)', [own]));
    rows.push(imp('a revoked blob', [CFG.revoked]));
    rows.push(imp('an unknown blob', ['blob:' + O + '/00000000-0000-4000-8000-000000000000']));
    rows.push(imp('data: text/javascript', ['data:text/javascript,self.L="data-js"']));
    rows.push(imp('data: no type', ['data:,self.L="data-plain"']));
    rows.push(imp('data: application/json', ['data:application/json,self.L="data-json"']));
    rows.push(imp('data: image/png', ['data:image/png,self.L="data-img"']));
    rows.push(imp('data: base64', ['data:text/javascript;base64,' + btoa('self.L="data-b64"')]));
    rows.push(imp('about:blank', ['about:blank']));
    rows.push(imp('javascript:', ['javascript:self.L="js-url"']));
    rows.push(imp('ftp:', ['ftp://127.0.0.1/x.js']));
    rows.push(imp('file:', ['file:///etc/hostname']));
    rows.push(imp('ws:', ['ws://127.0.0.1:' + P + '/x.js']));
    rows.push(imp('an invalid URL', ['http://[']));
    rows.push(imp('a relative URL', [js('self.L="relative"')]));
    rows.push(imp('http text/javascript', [O + js('self.L="js"')]));
    rows.push(imp('http application/javascript', [O + js('self.L="appjs"', { ct: 'application/javascript' })]));
    rows.push(imp('http text/plain', [O + js('self.L="plain"', { ct: 'text/plain' })]));
    rows.push(imp('http no Content-Type', [O + js('self.L="none"', { ct: 'none' })]));
    rows.push(imp('http text/html', [O + js('self.L="html"', { ct: 'text/html' })]));
    rows.push(imp('http application/json', [O + js('self.L="json"', { ct: 'application/json' })]));
    rows.push(imp('http image/png', [O + js('self.L="png"', { ct: 'image/png' })]));
    rows.push(imp('http text/javascript nosniff', [O + js('self.L="js-nosniff"', { xcto: 1 })]));
    rows.push(imp('http text/plain nosniff', [O + js('self.L="plain-nosniff"', { ct: 'text/plain', xcto: 1 })]));
    rows.push(imp('http 404', [O + js('self.L="404"', { st: 404 })]));
    rows.push(imp('http 500', [O + js('self.L="500"', { st: 500 })]));
    rows.push(imp('http 204', [O + js('', { st: 204 })]));
    rows.push(imp('a same-origin redirect', [O + '/redir?to=' + encodeURIComponent(O + js('self.L="redirected"'))]));
    rows.push(imp('cross-origin', [LH + js('self.L="cross"')]));
    rows.push(imp('a syntax error', [O + js('self.L = ;')]));
    rows.push(imp('a throw', [O + js('throw new RangeError("boom")')]));
    rows.push(imp('a cross-origin throw', [LH + js('throw new RangeError("boom-x")')]));
    rows.push(imp('a cross-origin syntax error', [LH + js('self.L = ;')]));
    var order = function (label, urls) {
      self.A = 0; self.B = 0;
      var r = imp(label, urls);
      return [r[0], r[1] + ' / A=' + self.A + ' B=' + self.B];
    };
    rows.push(order('order: 404 in the middle', [O + js('self.A=1'), O + js('', { st: 404 }), O + js('self.B=1')]));
    rows.push(order('order: an invalid URL last', [O + js('self.A=1'), 'http://[']));
    rows.push(order('order: about:blank in the middle', [O + js('self.A=1'), 'about:blank', O + js('self.B=1')]));
    rows.push(imp('an object with toString', [{ toString: function () { return O + js('self.L="tostring"'); } }]));
    try {
      importScripts(O + js('self.F = function () { return new Error("x").stack.split("\\n")[1]; }'));
      rows.push(['an imported function\'s stack frame', norm(self.F()).replace(/js\?b=[^:]*/, 'js?b=...')]);
    } catch (e) { rows.push(['an imported function\'s stack frame', err(e)]); }
    try {
      importScripts(O + '/redir?to=' + encodeURIComponent(O + js('self.G = function () { return new Error("x").stack.split("\\n")[1]; }')));
      rows.push(['a redirected import\'s stack frame', norm(self.G()).replace(/\?[^:]*:/, '?...:')]);
    } catch (e) { rows.push(['a redirected import\'s stack frame', err(e)]); }
    try {
      importScripts(O + js('self.H = function () { return new Error("x").stack.split("\\n")[1]; }') + '#frag');
      rows.push(['an import with a fragment: its stack frame', norm(self.H()).replace(/\?[^#:]*/, '?...')]);
    } catch (e) { rows.push(['an import with a fragment: its stack frame', err(e)]); }
    // nested workers (from this worker): the same construction checks, the strict MIME check for a classic script
    function nested(label, url, opts) {
      return new Promise(function (res) {
        var w;
        try { w = opts ? new Worker(url, opts) : new Worker(url); } catch (e) { return res([label, 'ctor ' + err(e)]); }
        w.onmessage = function (e) { res([label, 'message ' + norm(e.data)]); w.terminate(); };
        w.onerror = function (e) { res([label, 'error ' + e.constructor.name + ' ' + norm(e.message || '')]); };
        setTimeout(function () { res([label, 'timeout']); }, 3000);
      });
    }
    var hasWorker = typeof Worker === 'function';
    var nest = !hasWorker ? Promise.resolve([['nested', 'no Worker here']]) : Promise.all([
      nested('nested: http text/javascript', O + js('postMessage("n-js")')),
      nested('nested: http text/plain', O + js('postMessage("n-plain")', { ct: 'text/plain' })),
      nested('nested: http no Content-Type', O + js('postMessage("n-none")', { ct: 'none' })),
      nested('nested: data:', 'data:text/javascript,postMessage("n-data " + self.origin)'),
      nested('nested: cross-origin', LH + js('postMessage("n-cross")')),
      nested('nested: about:blank', 'about:blank'),
      nested('nested: invalid', 'http://['),
      nested('nested: the page\'s blob', URL.createObjectURL(new Blob(['postMessage("n-blob " + self.origin)'])))
    ]);
    return nest.then(function (n) { return rows.concat(n); });
  }

  function run(label, make) {
    return new Promise(function (res) {
      var t;
      try { t = make(); } catch (e) { return res([[label, 'ctor threw ' + e.name + ' | ' + norm(e.message)]]); }
      var on = function (e) { var d = e.data; res(Array.isArray(d) ? d.map(function (r) { return [label + ': ' + r[0], r[1]]; }) : [[label, String(d)]]); };
      if (t.port) { t.port.onmessage = on; } else { t.onmessage = on; }
      t.onerror = function (e) { res([[label, 'error ' + e.constructor.name + ' ' + norm(e.message || '')]]); };
      setTimeout(function () { res([[label, 'timeout']]); }, 15000);
    });
  }
  var CFG = JSON.stringify({ O: O, LH: LH, P: P, pageBlob: pageBlob, revoked: revoked });
  var body = '(' + workerMain + ')(' + CFG + ')';
  var dedicated = body + '.then(function (r) { postMessage(r); });';
  var shared = 'var R = ' + body + '; onconnect = function (ev) { var p = ev.ports[0]; R.then(function (r) { p.postMessage(r); }); };';
  var module = 'var r; try { importScripts(' + JSON.stringify(O + js('self.L=1')) + '); r = "ok"; } catch (e) { r = "threw " + e.name + " | " + e.constructor.name + " | " + e.message; } postMessage(r);';

  // the constructors, from the page
  function ctor(label, url, opts, shared) {
    return new Promise(function (res) {
      var w;
      try { w = shared ? new SharedWorker(url) : (opts ? new Worker(url, opts) : new Worker(url)); } catch (e) { return res([label, 'ctor threw ' + e.name + ' | ' + norm(e.message)]); }
      var on = function (e) { res([label, 'message ' + norm(e.data)]); };
      if (shared) w.port.onmessage = on; else w.onmessage = on;
      w.onerror = function (e) { res([label, 'error ' + e.constructor.name + ' ' + norm(e.message || '')]); };
      setTimeout(function () { res([label, 'timeout']); }, 4000);
    });
  }
  var post = function (s) { return 'postMessage(' + JSON.stringify(s) + ' + " " + self.origin + " " + location.href)'; };
  var sharedPost = function (s) { return 'onconnect = function (e) { e.ports[0].postMessage(' + JSON.stringify(s) + ' + " " + self.origin); }'; };
  var ctors = [
    ctor('Worker: an invalid URL', 'http://['),
    ctor('Worker: cross-origin', LH + js(post('x'))),
    ctor('Worker: https, same host', 'https://127.0.0.1:' + P + js(post('x'))),
    ctor('Worker: about:blank', 'about:blank'),
    ctor('Worker: javascript:', 'javascript:postMessage(1)'),
    ctor('Worker: ftp:', 'ftp://127.0.0.1/w.js'),
    ctor('Worker: ws:', 'ws://127.0.0.1:' + P + '/w.js'),
    ctor('Worker: data: text/javascript', 'data:text/javascript,' + post('data-js')),
    ctor('Worker: data: no type', 'data:,' + post('data-plain')),
    ctor('Worker: data: image/png', 'data:image/png,' + post('data-img')),
    ctor('Worker: the page\'s blob', URL.createObjectURL(new Blob([post('blob')]))),
    ctor('Worker: a revoked blob', revoked),
    ctor('Worker: an unknown blob', 'blob:' + O + '/00000000-0000-4000-8000-000000000000'),
    ctor('Worker: a relative URL', js(post('relative'))),
    ctor('Worker: http text/plain', O + js(post('plain'), { ct: 'text/plain' })),
    ctor('Worker: http no Content-Type', O + js(post('none'), { ct: 'none' })),
    ctor('Worker: http text/html', O + js(post('html'), { ct: 'text/html' })),
    ctor('Worker: http image/png', O + js(post('png'), { ct: 'image/png' })),
    ctor('Worker: http text/csv', O + js(post('csv'), { ct: 'text/csv' })),
    ctor('Worker: http text/plain nosniff', O + js(post('plain-nosniff'), { ct: 'text/plain', xcto: 1 })),
    ctor('Worker: http 404', O + js(post('404'), { st: 404 })),
    ctor('Worker: a same-origin redirect', O + '/redir?to=' + encodeURIComponent(O + js(post('redirected')))),
    ctor('Worker: a cross-origin redirect', O + '/redir?to=' + encodeURIComponent(LH + js(post('redirected-x')))),
    ctor('module Worker: data:', 'data:text/javascript,' + post('m-data'), { type: 'module' }),
    ctor('module Worker: the page\'s blob', URL.createObjectURL(new Blob([post('m-blob')], { type: 'text/javascript' })), { type: 'module' }),
    ctor('module Worker: a blob with no type', URL.createObjectURL(new Blob([post('m-blob-untyped')])), { type: 'module' }),
    ctor('module Worker: http text/javascript', O + js(post('m-js')), { type: 'module' }),
    ctor('module Worker: http text/plain', O + js(post('m-plain'), { ct: 'text/plain' }), { type: 'module' }),
    ctor('module Worker: cross-origin', LH + js(post('m-cross'), { acao: 1 }), { type: 'module' }),
    ctor('module Worker: importScripts', URL.createObjectURL(new Blob([module], { type: 'text/javascript' })), { type: 'module' }),
    ctor('SharedWorker: an invalid URL', 'http://[', null, true),
    ctor('SharedWorker: cross-origin', LH + js(sharedPost('x')), null, true),
    ctor('SharedWorker: about:blank', 'about:blank', null, true),
    ctor('SharedWorker: data:', 'data:text/javascript,' + sharedPost('s-data'), null, true),
    ctor('SharedWorker: http text/plain', O + js(sharedPost('s-plain'), { ct: 'text/plain' }), null, true)
  ];
  return Promise.all(ctors).then(function (c) {
    return run('blob Worker', function () { return new Worker(URL.createObjectURL(new Blob([dedicated], { type: 'text/javascript' }))); })
      .then(function (a) {
        return run('URL Worker', function () { return new Worker(js(dedicated)); }).then(function (b) {
          return run('blob SharedWorker', function () { return new SharedWorker(URL.createObjectURL(new Blob([shared], { type: 'text/javascript' }))); })
            .then(function (s) { return c.concat(a, b, s); });
        });
      });
  });
})()
