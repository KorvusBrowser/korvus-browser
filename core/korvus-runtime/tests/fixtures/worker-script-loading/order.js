(function () {
  // F3: a worker's start order. Each worker is made at the head of a 150 ms task; a URL worker's script is answered 300 ms
  // after it is asked for. Rows: when the server got the script request (ms after `new Worker`, rounded to 25 ms), when
  // the worker's time origin is (ms after `new Worker`, rounded to 25 ms), and whether its first statement ran within
  // 50 ms of its time origin.
  function busy(ms) { var t = performance.now(); while (performance.now() - t < ms); }
  function r25(x) { return Math.round(x / 25) * 25; }
  var src = 'postMessage([performance.timeOrigin, performance.now()])';
  function one(label, make, path) {
    return new Promise(function (res) {
      var w0 = Date.now(), t0 = performance.now(), w = make();
      busy(150);
      w.onmessage = function (e) {
        var origin = e.data[0] - (performance.timeOrigin + t0);
        var done = function (req) {
          res([label, 'request ' + (req === null ? 'none' : '+' + r25(req)), 'origin +' + r25(origin), 'first statement ' + (e.data[1] < 50 ? 'at once' : 'late')]);
        };
        if (!path) return done(null);
        fetch('/count').then(function (r) { return r.json(); }).then(function (seen) {
          var k = Object.keys(seen).filter(function (k) { return k.indexOf(path) >= 0; })[0];
          done(k ? seen[k][0].t - w0 : null);
        });
      };
      w.onerror = function () { res([label, 'error']); };
      setTimeout(function () { res([label, 'timeout']); }, 5000);
    });
  }
  var q = '?b=' + encodeURIComponent(src) + '&d=300';
  return one('blob classic', function () { return new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' }))); }, null)
    .then(function (a) {
      return one('URL classic', function () { return new Worker('/js' + q + '&k=c'); }, '&k=c').then(function (b) {
        return one('URL module', function () { return new Worker('/js' + q + '&k=m', { type: 'module' }); }, '&k=m').then(function (c) {
          return one('SharedWorker URL', function () {
            var s = new SharedWorker('/js?b=' + encodeURIComponent('onconnect = function (e) { e.ports[0].postMessage([performance.timeOrigin, performance.now()]); }') + '&d=300&k=s');
            return { set onmessage(f) { s.port.onmessage = f; }, set onerror(f) { s.onerror = f; } };
          }, '&k=s').then(function (d) { return [a, b, c, d]; });
        });
      });
    });
})()
