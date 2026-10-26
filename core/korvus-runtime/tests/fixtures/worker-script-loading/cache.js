(function () {
  // F3 (N11): which HTTP-cache partition a worker's requests use, and what they say about their creator. The page and a
  // cross-site frame (http://localhost) each load a cacheable script with <script>; then each starts a worker that
  // importScripts() it (one fetch per partition in Chrome), the frame first fetch()es a cacheable worker script and then
  // starts a worker from it, and starts a blob worker that imports the script by absolute URL. The server counts the
  // requests per host and records the headers of the frame's own <script>, and of the frame worker's uncached script and
  // import.
  var P = location.port, LH = 'http://localhost:' + P;
  function js(body, opt) {
    var q = '?b=' + encodeURIComponent(body);
    for (var k in (opt || {})) q += '&' + k + '=' + encodeURIComponent(opt[k]);
    return '/js' + q;
  }
  var LIB = js('self.__lib = (self.__lib || 0) + 1;', { cc: 'public, max-age=3600', k: 'lib' });
  var W = js('importScripts(' + JSON.stringify(LIB) + '); postMessage("w " + self.__lib);', { k: 'w' });
  var WC = js('importScripts(' + JSON.stringify(LIB) + '); postMessage("wc " + self.__lib);', { cc: 'public, max-age=3600', k: 'wc' });
  var WN = js('importScripts(' + JSON.stringify(js('self.__n = 1;', { k: 'imp' })) + '); postMessage("wn " + self.__n);', { k: 'wn' });
  function script(u) { return new Promise(function (res) { var s = document.createElement('script'); s.src = u; s.onload = function () { res('load'); }; s.onerror = function () { res('error'); }; document.head.appendChild(s); }); }
  function worker(u) { return new Promise(function (res) { var w = new Worker(u); w.onmessage = function (e) { res(String(e.data)); }; w.onerror = function () { res('worker error'); }; setTimeout(function () { res('timeout'); }, 5000); }); }
  // the frame's script: its own copy of the helpers, then the same steps, its answers posted to the page
  var frameMain = function (LIB, WC, WN, LH) {
    function worker(u) { return new Promise(function (res) { var w = new Worker(u); w.onmessage = function (e) { res(String(e.data)); }; w.onerror = function () { res('worker error'); }; setTimeout(function () { res('timeout'); }, 5000); }); }
    var out = [];
    fetch(WC).then(function (r) { return r.text(); }).then(function () { return worker(WC); })
      .then(function (r) { out.push(r); return worker(WN); })
      .then(function (r) {
        out.push(r);
        var src = 'importScripts(' + JSON.stringify(LH + LIB) + '); postMessage("wb " + self.__lib);';
        return worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
      })
      .then(function (r) { out.push(r); parent.postMessage(['f-done'].concat(out), '*'); });
  };
  var frameHtml = '<!doctype html><script src="' + LIB + '"></' + 'script><' + 'script>(' + frameMain + ')(' + JSON.stringify(LIB) + ',' + JSON.stringify(WC) + ',' + JSON.stringify(WN) + ',' + JSON.stringify(LH) + ')</' + 'script>';
  function frame() {
    return new Promise(function (res) {
      addEventListener('message', function (e) { if (e.data && e.data[0] === 'f-done') res(e.data.slice(1).join(', ')); });
      var f = document.createElement('iframe'); f.src = LH + '/html?b=' + encodeURIComponent(frameHtml); document.body.appendChild(f);
      setTimeout(function () { res('timeout'); }, 9000);
    });
  }
  var rows = [];
  function count(seen, host, path) { return (seen[host + path] || []).length; }
  function hdr(seen, host, path) {
    var h = (seen[host + path] || [])[0];
    if (!h) return 'not requested';
    return [h['sec-fetch-dest'], h['sec-fetch-mode'], h['sec-fetch-site'], 'referer ' + (h['referer'] || 'none').replace(/\?b=.*$/, '?b=...')].join(' / ');
  }
  return script(LIB).then(function (r) { rows.push(['page <script>', r]); return worker(W); })
    .then(function (r) { rows.push(['page worker importScripts', r]); return frame(); })
    .then(function (r) { rows.push(['frame workers', r]); return fetch('/count').then(function (x) { return x.json(); }); })
    .then(function (seen) {
      rows.push(['127.0.0.1 script requests', count(seen, '127.0.0.1', LIB)]);
      rows.push(['localhost script requests', count(seen, 'localhost', LIB)]);
      rows.push(['localhost cacheable worker script requests', count(seen, 'localhost', WC)]);
      rows.push(['frame <script> request', hdr(seen, 'localhost', LIB)]);
      rows.push(['frame worker script request', hdr(seen, 'localhost', WN)]);
      rows.push(['frame worker importScripts request', hdr(seen, 'localhost', js('self.__n = 1;', { k: 'imp' }))]);
      rows.push(['page worker script request', hdr(seen, '127.0.0.1', W)]);
      return rows;
    });
})()
