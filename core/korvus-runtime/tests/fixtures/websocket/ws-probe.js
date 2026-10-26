(async function () {
  /* The WebSocket fixture probe (tests/websocket_live.rs). Every case is one or more ROWS:
       [case, ...fields]                                  a synchronous answer
       [case, 'ev', name, ms, ...fields]                  an event, `ms` since the case began
     The comparison takes everything but `ms` exactly and `ms` as a bound. `__WSPORT__` is the fixture
     server's port (ws_server.py), substituted by whoever serves this probe. */
  var P = '__WSPORT__', B = 'ws://127.0.0.1:' + P;
  var rows = [];
  function err(e) { return e && e.name ? e.name + ': ' + e.message : 'THROW ' + String(e); }
  function T(f) { try { return f(); } catch (e) { return err(e); } }
  function wait(ms) { return new Promise(function (r) { setTimeout(r, ms); }); }
  function ab2arr(b) { return Array.prototype.slice.call(new Uint8Array(b)); }
  function desc(d) {
    if (typeof d === 'string') return d.length > 300 ? 'string(len=' + d.length + ')' : 'string:' + d;
    if (d instanceof ArrayBuffer) return 'ArrayBuffer:' + ab2arr(d).join(',');
    if (typeof Blob !== 'undefined' && d instanceof Blob) return 'Blob:' + d.size + ':' + JSON.stringify(d.type);
    return typeof d + ':' + Object.prototype.toString.call(d);
  }

  /* Run one socket to completion: record every event in order, call `act(ws, rec)` at open. */
  function run(name, url, protocols, opts) {
    opts = opts || {};
    return new Promise(function (res) {
      var t0 = performance.now(), ws, done = false, timer = 0;
      function ms() { return Math.round(performance.now() - t0); }
      function rec() { var a = [name, 'ev'].concat(Array.prototype.slice.call(arguments)); rows.push(a); }
      function finish() { if (done) return; done = true; clearTimeout(timer); res(ws); }
      try { ws = protocols === undefined ? new WebSocket(url) : new WebSocket(url, protocols); }
      catch (e) { rows.push([name, 'ctor-throw', err(e)]); finish(); return; }
      if (opts.binaryType) ws.binaryType = opts.binaryType;
      rows.push([name, 'ctor', ws.readyState, ws.url, ws.protocol, ws.extensions, ws.binaryType, ws.bufferedAmount]);
      if (opts.afterCtor) opts.afterCtor(ws, rec, ms);
      ws.onopen = function (e) {
        rec('open', ms(), ws.readyState, ws.protocol, ws.extensions, e.constructor.name, e.isTrusted);
        if (opts.onopen) opts.onopen(ws, rec, ms);
      };
      ws.onmessage = function (e) {
        rec('message', ms(), desc(e.data), e.constructor.name, e.origin === B, e.lastEventId, e.source, e.ports.length, e.isTrusted);
        if (opts.onmessage) opts.onmessage(ws, e, rec, ms);
      };
      ws.onerror = function (e) { rec('error', ms(), ws.readyState, e.constructor.name, e.isTrusted); };
      ws.onclose = function (e) {
        rec('close', ms(), ws.readyState, e.constructor.name, e.code, e.reason, e.wasClean, e.isTrusted, ws.bufferedAmount);
        setTimeout(finish, 30);
      };
      timer = setTimeout(function () { rec('timeout', ms(), ws.readyState); try { ws.close(); } catch (e) {} finish(); }, opts.limit || 4000);
    });
  }

  /* ---- the constructor's synchronous answers ---------------------------------------------------- */
  rows.push(['ctor-noargs', T(function () { return new WebSocket(); })]);
  rows.push(['ctor-call', T(function () { return WebSocket(B + '/echo'); })]);
  rows.push(['ctor-ftp', T(function () { return new WebSocket('ftp://127.0.0.1/'); })]);
  rows.push(['ctor-fragment', T(function () { return new WebSocket(B + '/echo#frag'); })]);
  rows.push(['ctor-empty-fragment', T(function () { return new WebSocket(B + '/echo#'); })]);
  rows.push(['ctor-invalid', T(function () { return new WebSocket('ws://[/'); })]);
  rows.push(['ctor-proto-space', T(function () { return new WebSocket(B + '/echo', 'a b'); })]);
  rows.push(['ctor-proto-empty', T(function () { return new WebSocket(B + '/echo', ''); })]);
  rows.push(['ctor-proto-dup', T(function () { return new WebSocket(B + '/echo', ['x', 'x']); })]);
  rows.push(['ctor-proto-nonascii', T(function () { return new WebSocket(B + '/echo', 'café\\'); })]);
  rows.push(['ctor-proto-comma', T(function () { return new WebSocket(B + '/echo', ['a,b']); })]);

  /* URL normalisation: no path, http(s) schemes, a relative URL against the page. Each is closed at
     once (close() in CONNECTING), so they also exercise that path's events. */
  /* Sockets closed while CONNECTING race the handshake (Chrome lets it run until the teardown task),
     so each gets a path of its own and the wire comparison leaves them out. */
  var norm = [B, 'http://127.0.0.1:' + P + '/norm?q=1', '/norm2', 'WS://127.0.0.1:' + P + '/a%20b'];
  for (var i = 0; i < norm.length; i++) {
    (function (u, k) {
      var r = T(function () {
        var ws = new WebSocket(u);
        var s = [ws.url, ws.readyState];
        ws.close();
        s.push(ws.readyState);
        return s.join(' | ');
      });
      rows.push(['url-' + k, r]);
    })(norm[i], i);
  }

  /* ---- attributes and methods off a live socket ------------------------------------------------- */
  await run('echo', B + '/echo', undefined, {
    onopen: function (ws, rec) {
      rec('send-connecting-ok', 0, T(function () { ws.send('hi'); return 'sent'; }), ws.bufferedAmount);
      setTimeout(function () { rec('buffered-after-task', 0, ws.bufferedAmount); }, 0);
    },
    onmessage: function (ws, e, rec) {
      rec('close-999', 0, T(function () { ws.close(999); }));
      rec('close-long-reason', 0, T(function () { ws.close(1000, new Array(125).join('x')); }));
      rec('close-clamp', 0, T(function () { ws.close(70000); }));
      rec('close-nan', 0, T(function () { ws.close('abc'); }));
      rec('close-1001', 0, T(function () { ws.close(1001); }));
      ws.close(1000, 'bye');
      rec('after-close', 0, ws.readyState, T(function () { ws.send('late'); return 'no-throw'; }), ws.bufferedAmount);
      rec('close-again', 0, T(function () { ws.close(4000); return 'no-throw'; }), ws.readyState);
    }
  });

  rows.push(['send-in-connecting', T(function () {
    var ws = new WebSocket(B + '/race/send');
    try { ws.send('x'); return 'no-throw'; } catch (e) { return err(e); } finally { ws.close(); }
  })]);

  rows.push(['binaryType', T(function () {
    var ws = new WebSocket(B + '/race/bt');
    var out = [ws.binaryType];
    ws.binaryType = 'arraybuffer'; out.push(ws.binaryType);
    ws.binaryType = 'foo'; out.push(ws.binaryType);
    ws.binaryType = 'blob'; out.push(ws.binaryType);
    ws.binaryType = 'ARRAYBUFFER'; out.push(ws.binaryType);
    ws.close();
    return out.join(',');
  })]);

  /* Binary echo: every BufferSource and a Blob, received as ArrayBuffer and as Blob. */
  var u8 = new Uint8Array([1, 2, 3, 250]);
  await run('binary-ab', B + '/echo', undefined, {
    binaryType: 'arraybuffer',
    onopen: function (ws, rec) {
      ws.send(u8.buffer);
      ws.send(new Uint8Array([9, 8, 7, 6, 5]).subarray(1, 3));
      ws.send(new DataView(new Uint8Array([4, 4]).buffer));
      ws.send(new Blob([new Uint8Array([66, 67])], { type: 'text/plain' }));
      ws.send('');
      rec('buffered', 0, ws.bufferedAmount);
      var n = 0;
      ws.addEventListener('message', function () { if (++n === 5) ws.send('__report__'); });
    },
    onmessage: function (ws, e, rec) {
      if (typeof e.data === 'string' && e.data.charAt(0) === '[') {
        var f = JSON.parse(e.data).map(function (x) { return [x.op, x.fin, x.rsv1, x.masked, x.len].join(':'); });
        rec('server-saw', 0, f.join(' '));
        ws.close();
      }
    }
  });
  await run('binary-blob', B + '/echo', undefined, {
    onopen: function (ws) { ws.send(u8); },
    onmessage: function (ws, e, rec) {
      var d = e.data;
      if (d instanceof Blob) {
        d.arrayBuffer().then(function (b) { rec('blob-bytes', 0, ab2arr(b).join(',')); ws.close(1000); });
      }
    }
  });

  /* A UTF-8 round trip, multi-byte and astral, and the bufferedAmount of a multi-byte string. */
  await run('utf8', B + '/echo', undefined, {
    onopen: function (ws, rec) { ws.send('hé€😀'); rec('buffered', 0, ws.bufferedAmount); },
    onmessage: function (ws) { ws.close(); }
  });

  /* Subprotocol selection and the extensions string. */
  await run('proto', B + '/proto', ['chat', 'superchat'], { onopen: function (ws) { ws.close(); } });
  await run('proto-none-offered', B + '/proto', [], { onopen: function (ws) { ws.close(); } });
  await run('deflate', B + '/deflate', undefined, {
    onopen: function (ws) { ws.send('compress me compress me compress me'); },
    onmessage: function (ws, e, rec) {
      if (e.data.charAt(0) === '[') {
        var f = JSON.parse(e.data).map(function (x) { return [x.op, x.fin, x.rsv1, x.masked].join(':'); });
        rec('server-saw', 0, f.join(' '));
        ws.close();
      } else ws.send('__report__');
    }
  });

  /* What the handshake request looked like on the wire: header NAMES in order, and the values that
     do not depend on the device. */
  await run('handshake', B + '/hdr?x=1', ['p1', 'p2'], {
    onmessage: function (ws, e, rec) {
      var h = JSON.parse(e.data);
      rec('request-line', 0, h.request_line);
      rec('names', 0, h.names.join(','));
      var v = h.values;
      ['Host', 'Connection', 'Pragma', 'Cache-Control', 'Upgrade', 'Origin', 'Sec-WebSocket-Version', 'Accept-Encoding',
       'Sec-WebSocket-Extensions', 'Sec-WebSocket-Protocol'].forEach(function (k) {
        rec('value', 0, k, v[k] === undefined ? '(absent)' : v[k].replace(/:\d+$/, ':PORT'));
      });
      rec('key-shape', 0, /^[A-Za-z0-9+/]{22}==$/.test(v['Sec-WebSocket-Key'] || ''));
      rec('has-ua', 0, typeof v['User-Agent'] === 'string' && v['User-Agent'] === navigator.userAgent);
      ws.close();
    }
  });

  /* Cookies ride the handshake. */
  document.cookie = 'wsfix=1; path=/';
  await run('cookie', B + '/cookie', undefined, {
    onmessage: function (ws, e, rec) { rec('cookie-has', 0, e.data.indexOf('wsfix=1') >= 0); ws.close(); }
  });

  /* Server-initiated close, a dropped connection, failed handshakes. */
  await run('server-close', B + '/close?code=4001&reason=srv', undefined, {});
  await run('server-close-1000', B + '/close?code=1000', undefined, {});
  await run('drop', B + '/drop', undefined, {});
  await run('status-404', B + '/status?code=404', undefined, {});
  await run('status-200', B + '/status?code=200', undefined, {});
  await run('badaccept', B + '/badaccept', undefined, {});
  await run('frag', B + '/frag', undefined, {
    binaryType: 'arraybuffer',
    onmessage: function (ws, e) { if (e.data instanceof ArrayBuffer) ws.close(); }
  });
  await run('badutf8', B + '/badutf8', undefined, {});
  await run('ping', B + '/ping', undefined, { onmessage: function (ws) { ws.close(); } });
  await run('close-in-connecting', B + '/race/cic', undefined, {
    afterCtor: function (ws, rec) { ws.close(); rec('after-close', 0, ws.readyState, ws.bufferedAmount); }
  });
  rows.push(['close-in-connecting-direct', await new Promise(function (res) {
    var ev = [], t0 = performance.now(), ws = new WebSocket(B + '/race/cicd');
    ['open', 'error', 'close'].forEach(function (n) {
      ws.addEventListener(n, function (e) { ev.push(n + (n === 'close' ? '(' + e.code + ',' + e.wasClean + ')' : '') + '@rs' + ws.readyState); });
    });
    ws.close();
    ev.push('after-close@rs' + ws.readyState);
    setTimeout(function () { res(ev.join(' ')); }, 300);
  })]);
  await run('slowclose', B + '/slowclose', undefined, { onopen: function (ws) { ws.close(1000, 'slow'); } });
  /* A message past the 64 KiB small-message threshold and past the 131000-byte data pipe: how the client
     frames it, and how bufferedAmount drains. */
  await run('large', B + '/echo', undefined, {
    onopen: function (ws, rec) {
      ws.send(new Array(200001).join('a'));
      rec('buffered', 0, ws.bufferedAmount);
      setTimeout(function () { rec('buffered-after-task', 0, ws.bufferedAmount); }, 0);
    },
    onmessage: function (ws, e, rec) {
      if (e.data.charAt(0) === '[') {
        var f = JSON.parse(e.data).map(function (x) { return [x.op, x.fin, x.rsv1, x.len].join(':'); });
        rec('server-saw', 0, f.join(' '));
        ws.close();
      } else { rec('echo-length', 0, e.data.length); ws.send('__report__'); }
    }
  });
  await run('client-close-no-code', B + '/echo', undefined, {
    onopen: function (ws) { ws.close(); }
  });
  await run('client-close-code-only', B + '/echo', undefined, {
    onopen: function (ws) { ws.close(3000); }
  });

  /* A refused connection: the BrowserScan port-scan shape (error then close, and when). */
  await run('refused', 'ws://127.0.0.1:47/', undefined, {});
  await run('refused-wss', 'wss://localhost:47/', undefined, {});
  await run('unsafe-port', 'ws://127.0.0.1:1/', undefined, {});

  /* The worker realm: WebSocket is [Exposed=(Window,Worker)]. */
  var wsrc = 'var B="' + B + '";var out=[typeof WebSocket];' +
    'var ws=new WebSocket(B+"/echo");ws.binaryType="arraybuffer";' +
    'ws.onopen=function(){out.push("open:"+ws.readyState);ws.send(new Uint8Array([7,7]));ws.send("w");};' +
    'ws.onmessage=function(e){out.push("message:"+(typeof e.data==="string"?e.data:new Uint8Array(e.data).join(","))+":"+(e.origin===B));if(typeof e.data==="string")ws.close(1000,"wk");};' +
    'ws.onerror=function(){out.push("error");};' +
    'ws.onclose=function(e){out.push("close:"+e.code+":"+e.reason+":"+e.wasClean);postMessage(out.join(" "));};';
  var wout = await new Promise(function (res) {
    try {
      var w = new Worker(URL.createObjectURL(new Blob([wsrc], { type: 'text/javascript' })));
      w.onmessage = function (e) { res(e.data); };
      w.onerror = function (e) { res('worker-error ' + (e.message || '')); };
      setTimeout(function () { res('worker-timeout'); }, 5000);
    } catch (e) { res(err(e)); }
  });
  rows.push(['worker', wout]);
  return rows;
})()
