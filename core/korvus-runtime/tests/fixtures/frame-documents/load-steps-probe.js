(async () => {
  // a frame document's end as the challenge frame's DOMContentLoaded handler sees it: [host, step, readyState, ...]
  const R = [];
  const port = location.port;
  const html = '<!doctype html><html><head><title>t</title></head><body><script>' +
    'var log = []; log.push(["script", document.readyState]);' +
    'document.addEventListener("DOMContentLoaded", function () {' +
    '  var n = performance.getEntriesByType("navigation")[0];' +
    '  log.push(["DOMContentLoaded", document.readyState, n.loadEventEnd > 0]);' +
    '  Promise.resolve().then(function () { log.push(["DOMContentLoaded microtask", document.readyState]); });' +
    '  setTimeout(function () { var m = performance.getEntriesByType("navigation")[0];' +
    '    log.push(["DOMContentLoaded timer", document.readyState, m.loadEventEnd > 0, m.duration > 0, m.duration === m.loadEventEnd]);' +
    '    parent.postMessage({log: log}, "*"); }, 0); });' +
    'window.addEventListener("load", function () { log.push(["window load", document.readyState, performance.getEntriesByType("navigation")[0].loadEventEnd > 0]); });' +
    '<\/script></body></html>';
  for (const host of ['127.0.0.1', 'localhost']) {
    const f = document.createElement('iframe');
    const order = [];
    const got = new Promise((res) => {
      f.addEventListener('load', () => order.push('element load'));
      window.addEventListener('message', function h(e) {
        if (e.source === f.contentWindow) { window.removeEventListener('message', h); order.push('report'); res(e.data.log); }
      });
    });
    f.src = 'http://' + host + ':' + port + '/doc?h=' + encodeURIComponent(html);
    document.body.appendChild(f);
    const log = await got;
    for (const l of log) R.push([host].concat(l));
    await new Promise((r) => setTimeout(r, 100));
    R.push([host, 'element load count', order.filter((o) => o === 'element load').length]);
    R.push([host, 'parent order', order.join(' > ')]);
    f.remove();
  }
  return R;
})()
