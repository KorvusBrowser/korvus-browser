(function () {
  // Error.stack, a page's Error.prepareStackTrace and its call sites, Error.captureStackTrace and Error.stackTraceLimit,
  // as V8 + Blink make them. Every case runs inside a blob MODULE (its URL read `blob:URL`), so its frames are module
  // positions; a frame of this page's own script reads the page URL. Line and column read `#line#col` (not `:line:col`)
  // so that the recording's port normalisation (`:` and 2-5 digits) leaves them as they are.
  var MAP = 'eyJ2ZXJzaW9uIjozLCJzb3VyY2VzIjpbIiJdLCJzb3VyY2VzQ29udGVudCI6WyIgIl0sIm5hbWVzIjpbXSwibWFwcGluZ3MiOiJBQUFBO0FBQUE7QUFBQTtBQUFBO0FBQUEifQ==';
  var SM = '\n//# sourceMappingURL=data:application/json;charset=utf-8;base64,' + MAP;
  function loc(s) {
    if (typeof s !== 'string') return s;
    return s.replace(/blob:http:\/\/127\.0\.0\.1(:\d+)?\/[0-9a-f-]{36}/g, 'blob:URL').replace(/127\.0\.0\.1:\d+/g, '127.0.0.1:PORT')
      .replace(/:(\d+)/g, '#$1');
  }
  window.__f8loc = loc;

  // ---- the module that carries every case but the source-mapped ones
  async function cases() {
    var loc = window.__f8loc;
    var rows = [];
    function row(label, f) {
      try { rows.push([label, f()]); } catch (e) { rows.push([label, 'THREW', String(e && e.name), loc(String(e && e.message)), loc(e && e.stack)]); }
    }
    function pstWith(f, body) {
      var saved = Error.prepareStackTrace;
      Error.prepareStackTrace = f;
      try { return body(); } finally { Error.prepareStackTrace = saved; }
    }
    function m(c, k) { return typeof c[k] === 'function' ? c[k]() : 'no ' + k; }
    function site(c) {
      var fn = m(c, 'getFunction'), th = m(c, 'getThis');
      return [loc(m(c, 'getFileName')), m(c, 'getLineNumber'), m(c, 'getColumnNumber'), m(c, 'getFunctionName'), m(c, 'getMethodName'),
        m(c, 'getTypeName'), typeof th, th === window, typeof fn, m(c, 'isToplevel'), m(c, 'isEval'), m(c, 'isNative'), m(c, 'isConstructor'),
        m(c, 'isAsync'), m(c, 'isPromiseAll'), m(c, 'getPromiseIndex'), loc(m(c, 'getScriptNameOrSourceURL')), loc(m(c, 'getEvalOrigin')),
        m(c, 'getEnclosingLineNumber'), m(c, 'getEnclosingColumnNumber'), m(c, 'getPosition'), typeof m(c, 'getScriptHash'), loc(m(c, 'toString')),
        loc(String(c))];
    }
    function sites(e, cs) { return cs.slice(0, 5).map(site); }

    // 1. frames
    function CI() { try { null[0](); } catch (e) { return e.stack; } }
    row('plain frames', function () { return loc((function wrap() { return CI(); })()); });
    function Foo() { this.s = new Error('ctor').stack; }
    row('constructor frame', function () { return loc(new Foo().s); });
    var o = { m: function () { return new Error('m').stack; }, get g() { return new Error('g').stack; } };
    o.alias = o.m;
    o[Symbol.iterator] = function () { return new Error('sym').stack; };
    row('method frame', function () { return loc(o.m()); });
    row('method frame [as alias]', function () { return loc(o.alias()); });
    row('getter frame', function () { return loc(o.g); });
    row('symbol method frame', function () { return loc(o[Symbol.iterator]()); });
    class K { static s() { return new Error('s').stack; } i() { return new Error('i').stack; } }
    row('class frames', function () { return [loc(K.s()), loc(new K().i())]; });
    row('builtin frames', function () {
      return [loc([1].map(function () { return new Error('map').stack; })[0]),
        loc('a'.replace(/a/, function () { return new Error('rep').stack; })),
        loc((function () { var s; new Promise(function () { s = new Error('p').stack; }); return s; })()),
        loc(JSON.parse('[1]', function (k, v) { return k === '' ? new Error('rev').stack : v; }))];
    });
    row('eval frames', function () {
      return [loc(eval('new Error("direct").stack')),
        loc((0, eval)('new Error("indirect").stack')),
        loc((0, eval)('new Error("named").stack\n//# sourceURL=zz.js')),
        loc(new Function('return new Error("fn").stack')()),
        loc(new Function('return new Error("fnnamed").stack\n//# sourceURL=ff.js')())];
    });
    row('sloppy frames (new Function)', function () {
      var g = new Function('cb', 'return (function inner() { return cb(); }).call({k: 1});');
      return loc(g(function () { return new Error('sloppy').stack; }));
    });
    row('stackTraceLimit 10 truncates', function () {
      function deep(n) { return n ? deep(n - 1) : new Error('deep').stack; }
      var s = deep(15);
      return [s.split('\n').length, loc(s.split('\n').slice(-1)[0])];
    });

    // 2. async frames
    async function a1() { await null; throw new Error('async'); }
    async function a2() { await a1(); }
    async function pa() { await Promise.all([1, a1()]); }
    try { await a2(); } catch (e) { rows.push(['async frames', loc(e.stack)]); }
    try { await pa(); } catch (e) { rows.push(['Promise.all frames', loc(e.stack)]); }
    var asyncSites = null;
    Error.prepareStackTrace = function (e, cs) { return sites(e, cs); };
    try { await pa(); } catch (e) { asyncSites = e.stack; }
    Error.prepareStackTrace = undefined;
    rows.push(['async call sites', asyncSites]);

    // 3. call sites
    row('call sites: module frames', function () { return pstWith(sites, function () { return (function wrap() { try { null[0](); } catch (e) { return e.stack; } })(); }); });
    row('call sites: sloppy frames', function () {
      return pstWith(sites, function () {
        var g = new Function('cb', 'return (function inner() { return cb(); }).call({k: 1});');
        return g(function () { return new Error('s').stack; });
      });
    });
    row('call sites: constructor and method', function () {
      return pstWith(sites, function () { return [new Foo().s, o.alias(), o.g]; });
    });
    row('call sites: builtin frames', function () {
      return pstWith(sites, function () { return [1].map(function () { return new Error('map').stack; })[0]; });
    });
    row('call site objects', function () {
      var cs0 = null;
      pstWith(function (e, cs) { cs0 = cs; return ''; }, function () { return new Error('x').stack; });
      var c = cs0[0], proto = Object.getPrototypeOf(c), d = Object.getOwnPropertyDescriptor(proto, 'getFileName');
      var foreign;
      try { proto.getFileName.call({}); foreign = 'no throw'; } catch (e) { foreign = [e.name, e.message]; }
      var made;
      try { made = typeof new proto.constructor(); } catch (e) { made = [e.name, e.message]; }
      return [Array.isArray(cs0), Object.isFrozen(cs0), Object.prototype.toString.call(c), Object.getOwnPropertyNames(proto).sort().join(','),
        Object.getOwnPropertyNames(c).length, Object.getOwnPropertySymbols(c).length, Object.getPrototypeOf(proto) === Object.prototype,
        typeof proto.constructor, proto.constructor && proto.constructor.name, proto.constructor && proto.constructor.length,
        [d.writable, d.enumerable, d.configurable], proto.getFileName.name, proto.getFileName.length, proto.toString.length,
        foreign, made, JSON.stringify(c), Object.keys(c).length, Object.getPrototypeOf(cs0[1]) === proto];
    });

    // 4. prepareStackTrace's protocol
    row('prepareStackTrace this, arguments', function () {
      var seen;
      var s = pstWith(function (e, cs) { seen = [this === Error, arguments.length, e instanceof Error, typeof cs]; return 'R'; }, function () { return new Error('x').stack; });
      return [s, seen];
    });
    row('prepareStackTrace returns an object', function () {
      var v = pstWith(function () { return { k: 1 }; }, function () { return new Error('x').stack; });
      return [typeof v, JSON.stringify(v)];
    });
    row('prepareStackTrace throws', function () {
      var e = new Error('x'), out = [];
      pstWith(function () { throw new RangeError('in pst'); }, function () {
        try { out.push(['first', String(e.stack)]); } catch (x) { out.push(['first threw', x.name, x.message]); }
        try { out.push(['second', loc(String(e.stack))]); } catch (x) { out.push(['second threw', x.name, x.message]); }
      });
      return out;
    });
    row('prepareStackTrace recursion formats the inner error by default', function () {
      return pstWith(function () { return 'outer|' + loc(new Error('inner').stack); }, function () { return new Error('x').stack; });
    });
    row('prepareStackTrace not a function', function () { return pstWith(42, function () { return loc(new Error('x').stack); }); });
    row('prepareStackTrace set after construction, before the first read', function () {
      var e = new Error('late');
      return pstWith(function () { return 'formatted late'; }, function () { return e.stack; });
    });
    row('prepareStackTrace read off the intrinsic Error, not the global', function () {
      var E0 = Error;
      window.Error = { prepareStackTrace: function () { return 'the global'; } };
      try { return loc(new E0('x').stack); } finally { window.Error = E0; }
    });
    row('prepareStackTrace for captureStackTrace on a plain object', function () {
      var t = {}, got;
      pstWith(function (e, cs) { got = [e === t, cs.length > 0]; return 'P'; }, function () { Error.captureStackTrace(t); return t.stack; });
      return [got, t.stack];
    });

    // 5. Error.captureStackTrace and Error.stackTraceLimit
    row('captureStackTrace on a plain object', function () {
      var t = {};
      var r = Error.captureStackTrace(t);
      var d = Object.getOwnPropertyDescriptor(t, 'stack');
      return [r, loc(t.stack), typeof d.get, typeof d.set, typeof d.value, d.enumerable, d.configurable, d.writable];
    });
    row('captureStackTrace skipping frames', function () {
      function skipme() { var t = {}; Error.captureStackTrace(t, skipme); return t.stack; }
      return loc((function caller() { return skipme(); })());
    });
    row('captureStackTrace name and message', function () {
      var t = { name: 'N', message: 'M' };
      Error.captureStackTrace(t);
      return loc(t.stack.split('\n')[0]);
    });
    row('captureStackTrace on a frozen object', function () { var t = Object.freeze({}); Error.captureStackTrace(t); return typeof t.stack; });
    row('own stack of an Error', function () {
      var e = new Error('x'), d = Object.getOwnPropertyDescriptor(e, 'stack');
      return [Object.getOwnPropertyNames(e).join(','), !!d, d && typeof d.get, d && typeof d.value, d && d.enumerable, d && d.configurable, d && d.writable];
    });
    row('stackTraceLimit and its descriptor', function () {
      var d = Object.getOwnPropertyDescriptor(Error, 'stackTraceLimit');
      return [Error.stackTraceLimit, d.writable, d.enumerable, d.configurable, Error.captureStackTrace.length, Error.captureStackTrace.name];
    });
    row('stackTraceLimit 0, 2, a string, deleted', function () {
      var out = [];
      Error.stackTraceLimit = 0; out.push(loc(new Error('z').stack));
      Error.stackTraceLimit = 2; out.push(loc(new Error('two').stack));
      Error.stackTraceLimit = 'x'; var e = new Error('s'); out.push([typeof e.stack, 'stack' in e]);
      delete Error.stackTraceLimit; e = new Error('d'); out.push([typeof e.stack, 'stack' in e]);
      Error.stackTraceLimit = 10;
      return out;
    });

    // 6. the error's own head line, read when stack is first read
    row('name and message changed before the first read', function () { var e = new Error('m'); e.name = 'Renamed'; e.message = 'changed'; return loc(e.stack.split('\n')[0]); });
    row('name changed after the first read', function () { var e = new Error('m'); var a = e.stack; e.name = 'Late'; return [a === e.stack, e.stack.split('\n')[0]]; });
    row('a symbol name', function () { var e = new Error('m'); e.name = Symbol('s'); return loc(e.stack.split('\n')[0]); });
    row('empty name, empty message', function () {
      var a = new Error(''); a.name = ''; var b = new Error('only message'); b.name = ''; var c = new Error(''); c.name = 'OnlyName';
      return [a.stack.split('\n')[0], b.stack.split('\n')[0], c.stack.split('\n')[0]];
    });
    row('subclass', function () { class MyErr extends Error { constructor(m) { super(m); this.name = 'MyErr'; } } return loc(new MyErr('sub').stack); });
    row('a thrown non-Error, captured', function () { var t = { toString: function () { return 'TS'; } }; Error.captureStackTrace(t); return t.stack.split('\n')[0]; });

    // 7. errors the platform throws
    function caught(f) { try { f(); return 'no throw'; } catch (e) { var d = Object.getOwnPropertyDescriptor(e, 'stack'); return [e.name, loc(e.stack), Object.getOwnPropertyNames(e).join(','), !!d && typeof d.get, !!d && typeof d.value]; } }
    row('DOMException from createElement', function () { return caught(function () { document.createElement('1'); }); });
    row('DOMException from querySelector', function () { return caught(function () { document.querySelector('<<<'); }); });
    row('DOMException from atob', function () { return caught(function () { atob('*'); }); });
    row('TypeError from appendChild', function () { return caught(function () { document.body.appendChild(5); }); });
    row('TypeError from new URL', function () { return caught(function () { new URL('nope'); }); });
    row('TypeError from an illegal invocation', function () { return caught(function () { Node.prototype.appendChild.call({}, document.body); }); });
    row('TypeError from a constructor called as a function', function () { return caught(function () { Event('x'); }); });
    row('a page-made DOMException has no stack', function () { var e = new DOMException('m'); return [e.stack === undefined, Object.getOwnPropertyNames(e).join(',')]; });
    row('platform DOMException through prepareStackTrace', function () {
      return pstWith(function (e, cs) { return [e.name, cs.length > 0, loc(cs[0].getFileName()), cs[0].getLineNumber(), cs[0].getColumnNumber()]; },
        function () { try { document.createElement('1'); } catch (e) { return e.stack; } });
    });

    // 8. another realm's Error.prepareStackTrace: the one of the realm the error was made in
    row('prepareStackTrace of the realm that made the error', function () {
      var f = document.createElement('iframe');
      document.body.appendChild(f);
      var W = f.contentWindow;
      Error.prepareStackTrace = function () { return 'top'; };
      W.Error.prepareStackTrace = function () { return 'frame'; };
      try {
        return [new W.Error('a').stack, W.eval('new Error("b")').stack, new Error('c').stack, W.eval('(function () { return new Error("d"); })')().stack];
      } finally { Error.prepareStackTrace = undefined; f.remove(); }
    });
    return rows;
  }

  // ---- source maps: a module carrying an inline map (every line -> source "" 1:1), and modules naming themselves
  async function mapped() {
    var loc = window.__f8loc;
    function CI() { try { null[0](); } catch (e) { return e.stack; } }
    var rows = [['module + inline source map', loc((function wrap() { return CI(); })())]];
    var saved = Error.prepareStackTrace;
    Error.prepareStackTrace = function (e, cs) { return cs.slice(0, 3).map(function (c) { return [loc(c.getFileName()), c.getLineNumber(), c.getColumnNumber(), loc(c.toString())]; }); };
    try { rows.push(['call sites in a module with an inline source map', (function wrap() { return CI(); })()]); } finally { Error.prepareStackTrace = saved; }
    try { (0, eval)('null.x\n//# sourceURL=evalmapped.js' + window.__f8sm); } catch (e) { rows.push(['eval with sourceURL and an inline map', loc(e.stack)]); }
    return rows;
  }
  async function named() {
    var loc = window.__f8loc;
    try { null[0](); } catch (e) { return [['module with a sourceURL', loc(e.stack)]]; }
  }
  window.__f8sm = SM;
  function blob(text) { return import(URL.createObjectURL(new Blob([text], { type: 'text/javascript' }))); }
  var rows = [];
  return blob('export const run = ' + cases.toString() + ';\n')
    .then(function (m) { return m.run(); })
    .then(function (r) { rows = rows.concat(r); return blob('export const run = ' + mapped.toString() + ';' + SM); })
    .then(function (m) { return m.run(); })
    .then(function (r) { rows = rows.concat(r); return blob('export const run = ' + named.toString() + ';\n//# sourceURL=named-module.js'); })
    .then(function (m) { return m.run(); })
    .then(function (r) { rows = rows.concat(r); return blob('export const run = ' + named.toString() + ';\n//# sourceURL=named-mapped.js' + SM); })
    .then(function (m) { return m.run(); })
    .then(function (r) { return rows.concat(r.map(function (x) { return [x[0] + ' and an inline map', x[1]]; })); },
      function (e) { rows.push(['FATAL', String(e), loc(e && e.stack)]); return rows; });
})()
