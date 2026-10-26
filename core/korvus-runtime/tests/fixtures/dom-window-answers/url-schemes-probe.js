(function () {
  // The URL parser over the schemes Chromium's GURL treats specially (Chrome 150): filesystem: URLs, the schemes
  // Chrome registers as standard, file:'s origin, relative resolution and the setters, through URL and <a href>.
  // Left out: the five schemes GURL parses as opaque paths, file://localhost, filesystem:file:<path> (see the test).
  var a = (function () {
  // The URL parser over the schemes Chromium's GURL treats specially: filesystem: (an inner URL), the
  // schemes Chrome registers as standard, the opaque-path non-special schemes, and controls.
  var rows = [];
  var keys = ['href', 'protocol', 'username', 'password', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'origin'];
  function rec(label, input, base) {
    try {
      var u = base === undefined ? new URL(input) : new URL(input, base);
      rows.push([label, 'ok'].concat(keys.map(function (k) { return u[k]; })));
    } catch (e) { rows.push([label, 'throw', e.name, e.message]); }
    try { rows.push([label + ' canParse', base === undefined ? URL.canParse(input) : URL.canParse(input, base)]); } catch (e) { rows.push([label + ' canParse', 'throw', e.name]); }
  }
  [
    'filesystem:http://example.com/temporary/a.txt',
    'filesystem:HTTP://EXAMPLE.COM:80/Temporary/a b.txt?q=1#h',
    'filesystem:http://example.com/temporary',
    'filesystem:http://example.com/temporary/',
    'filesystem:http://example.com/',
    'filesystem:http://example.com',
    'filesystem:http://user:pw@example.com:8080/persistent/x/../y',
    'filesystem:https://example.com/temporary/a',
    'filesystem:file:///temporary/a',
    'filesystem:file://host/temporary/a',
    'filesystem:ftp://e.com/temporary/a',
    'filesystem:ws://e.com/temporary/a',
    'filesystem:blob:http://e.com/x',
    'filesystem:filesystem:http://e.com/temporary/a',
    'filesystem:mailto:x',
    'filesystem:chrome-extension://abc/temporary/a',
    'filesystem:about:blank',
    'filesystem:',
    'filesystem:///default.ini',
    'filesystem:///default.ini?#1234567890',
    'FileSystem:http://e.com/temporary/a',
    ' filesystem:http://e.com/temporary/a ',
    'filesystem:http://e.com/temporary/a\\b',
    'filesystem:http://e.com\\temporary\\a',
    'filesystem:http://e.com/temporary/%41%zz',
    'filesystem:http://[::1]/temporary/a',
    'filesystem:http://e.com:99999/temporary/a',
    'chrome-extension://ABCDEF/path/../x?y#z',
    'chrome-extension://abc',
    'chrome-extension://abc:80/x',
    'chrome-extension://u:p@abc/x',
    'chrome-extension:abc/x',
    'chrome-extension:///abc/x',
    'chrome-extension://abc\\x\\y',
    'chrome://settings',
    'chrome://Settings/a/./b',
    'devtools://devtools/bundled/x.html',
    'chrome-untrusted://x/y',
    'chrome-error://chromewebdata/',
    'isolated-app://abc/x',
    'chrome-native://newtab/',
    'chrome-search://local-ntp/x',
    'chrome-distiller://abc/x',
    'foo://Host/x/../y',
    'javascript:alert(1)',
    'data:text/plain,x',
    'blob:http://e.com/uuid',
    'about:blank',
    'mailto:A@B.c',
    'ws://E.com/x',
    'file://HOST/x',
    'http://e.com:80/'
  ].forEach(function (s) { rec(s, s); });
  [
    ['b.txt', 'filesystem:http://e.com/temporary/a/c.txt'],
    ['/b', 'filesystem:http://e.com/temporary/a'],
    ['?q', 'filesystem:http://e.com/temporary/a'],
    ['#h', 'filesystem:http://e.com/temporary/a'],
    ['http://x.com/', 'filesystem:http://e.com/temporary/a'],
    ['b', 'filesystem:///default.ini'],
    ['../x', 'chrome-extension://abc/a/b'],
    ['x', 'chrome://settings/a'],
    ['//other/x', 'chrome-extension://abc/a']
  ].forEach(function (p) { rec(p[0] + ' against ' + p[1], p[0], p[1]); });
  function setr(label, input, key, value) {
    try { var u = new URL(input); u[key] = value; rows.push([label, u.href, u.pathname, u.host]); } catch (e) { rows.push([label, 'throw', e.name, e.message]); }
  }
  setr('fs pathname=', 'filesystem:http://e.com/temporary/a', 'pathname', '/b/c');
  setr('fs search=', 'filesystem:http://e.com/temporary/a', 'search', 'x=1');
  setr('fs hash=', 'filesystem:http://e.com/temporary/a', 'hash', 'z');
  setr('fs host=', 'filesystem:http://e.com/temporary/a', 'host', 'other.com');
  setr('fs protocol=http', 'filesystem:http://e.com/temporary/a', 'protocol', 'http');
  setr('http protocol=filesystem', 'http://e.com/temporary/a', 'protocol', 'filesystem');
  setr('ext host=', 'chrome-extension://abc/x', 'host', 'DEF');
  setr('ext pathname=', 'chrome-extension://abc/x', 'pathname', 'y/../z');
  setr('ext port=', 'chrome-extension://abc/x', 'port', '8080');
  setr('ext protocol=http', 'chrome-extension://abc/x', 'protocol', 'http');
  try { rows.push(['URL.parse fs', String(URL.parse('filesystem:///x')), String(URL.parse('filesystem:http://e.com/t/a'))]); } catch (e) { rows.push(['URL.parse fs', 'throw', e.name]); }
  var a = document.createElement('a');
  a.href = 'filesystem:http://e.com/temporary/a.txt';
  rows.push(['anchor fs', a.href, a.protocol, a.host, a.pathname, a.origin]);
  a.href = 'filesystem:///default.ini';
  rows.push(['anchor bad fs', a.href, a.protocol, a.pathname, a.origin]);
  a.href = 'chrome-extension://ABC/x';
  rows.push(['anchor ext', a.href, a.host, a.origin]);
  return rows;
})();
  var b = (function () {
  var rows = [];
  var keys = ['href', 'protocol', 'host', 'pathname', 'search', 'hash', 'origin'];
  function rec(label, input, base) {
    try { var u = base === undefined ? new URL(input) : new URL(input, base); rows.push([label, 'ok'].concat(keys.map(function (k) { return u[k]; }))); }
    catch (e) { rows.push([label, 'throw', e.name, e.message]); }
  }
  ['blob:file:///x', 'blob:filesystem:http://e.com/temporary/a', 'blob:chrome-extension://abc/x', 'file:///x',
   'chrome-extension://abc:xyz/x', 'chrome-extension://abc:99999/x', 'chrome://', 'chrome:///', 'chrome-extension://%41bc/x',
   'chrome-extension://abc/x?a\'b', 'chrome-extension://abc/a b', 'devtools:bundled', 'filesystem:http://e.com/temporary/../a',
   'filesystem:http://e.com/temporary/a/../../b', 'filesystem:http://e.com/a%2Fb/c', 'filesystem:file:/temporary/a',
   'filesystem:http:e.com/temporary/a', 'filesystem:chrome-extension:abc/temporary/a', 'filesystem:http://e.com/temporary/a?x#y?z'].forEach(function (s) { rec(s, s); });
  [['//other/x', 'filesystem:http://e.com/temporary/a'], ['\\\\other\\x', 'filesystem:http://e.com/temporary/a'], ['b\\c', 'filesystem:http://e.com/temporary/a/x'],
   ['chrome-extension:x', 'chrome-extension://abc/a/b'], ['chrome-extension:/x', 'chrome-extension://abc/a/b'], ['..\\y', 'chrome-extension://abc/a/b'],
   ['//OTHER:5/x', 'chrome-extension://abc/a'], ['filesystem:b', 'filesystem:http://e.com/temporary/a'], ['', 'filesystem:http://e.com/temporary/a?q#h'],
   ['', 'chrome-extension://abc/a?q#h']].forEach(function (p) { rec(JSON.stringify(p[0]) + ' against ' + p[1], p[0], p[1]); });
  function setr(label, input, key, value) {
    try { var u = new URL(input); u[key] = value; rows.push([label, u.href, u.protocol, u.host, u.pathname, u.origin]); } catch (e) { rows.push([label, 'throw', e.name, e.message]); }
  }
  setr('foo protocol=http', 'foo://x/y', 'protocol', 'http');
  setr('foo protocol=https', 'foo://h:8/y?q', 'protocol', 'https');
  setr('mailto protocol=http', 'mailto:a@b', 'protocol', 'http');
  setr('data protocol=chrome', 'data:x', 'protocol', 'chrome');
  setr('foo protocol=chrome-extension', 'foo://X/y', 'protocol', 'chrome-extension');
  setr('http protocol=chrome-extension', 'http://x/y', 'protocol', 'chrome-extension');
  setr('ext protocol=chrome', 'chrome-extension://abc/x', 'protocol', 'chrome');
  setr('ext protocol=foo', 'chrome-extension://abc/x', 'protocol', 'foo');
  setr('ext hostname=', 'chrome-extension://abc/x', 'hostname', 'XY.Z');
  setr('ext host=with port', 'chrome-extension://abc/x', 'host', 'q:9');
  setr('ext host=empty', 'chrome-extension://abc/x', 'host', '');
  setr('ext username=', 'chrome-extension://abc/x', 'username', 'u');
  setr('ext search=', 'chrome-extension://abc/x', 'search', 'a b');
  setr('ext hash=', 'chrome-extension://abc/x', 'hash', 'a b');
  setr('fs pathname=rel', 'filesystem:http://e.com/temporary/a', 'pathname', 'b');
  setr('fs pathname=dots', 'filesystem:http://e.com/temporary/a', 'pathname', '/x/../y');
  setr('fs pathname=empty', 'filesystem:http://e.com/temporary/a', 'pathname', '');
  setr('fs protocol=https', 'filesystem:http://e.com/temporary/a', 'protocol', 'https');
  setr('fs protocol=foo', 'filesystem:http://e.com/temporary/a', 'protocol', 'foo');
  setr('fs href=bad', 'filesystem:http://e.com/temporary/a', 'href', 'filesystem:x');
  setr('fs search=empty', 'filesystem:http://e.com/temporary/a?q', 'search', '');
  setr('fs username=', 'filesystem:http://e.com/temporary/a', 'username', 'u');
  setr('fs port=', 'filesystem:http://e.com/temporary/a', 'port', '81');
  try { var u = new URL('filesystem:http://e.com/temporary/a?x=1'); rows.push(['fs searchParams', u.searchParams.get('x'), JSON.stringify(u.toJSON())]); } catch (e) { rows.push(['fs searchParams', 'throw', e.name]); }
  return rows;
})();
  return a.concat(b);
})()
