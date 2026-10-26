(async function () {
  // ABPROG phase 3, F9: document.fonts as Blink's FontFaceSetDocument keeps it -- the @font-face rules of the
  // document's sheets as CSS-connected FontFaces (in the set, before the faces script added; not deletable), their
  // statuses as layout and FontFaceSet.load() use them, the loading / loadingdone / loadingerror events, check(),
  // ready. Every /f9-*.woff2 answers 404. No row reads a face's status right after a forced layout (Chrome loads
  // there; this engine loads where its style-induced loads run), and no event row records the document's readyState.
  var out = [], fs = document.fonts, log = [], readyLog = [], loadFired = false;
  function row(k, v) { out.push([k, v]); }
  function faces() { var s = []; fs.forEach(function (f) { s.push([f.family, f.style, f.weight, f.stretch, f.unicodeRange, f.display, f.status].join('/')); }); return s.join(','); }
  ['loading', 'loadingdone', 'loadingerror'].forEach(function (t) {
    fs.addEventListener(t, function (e) {
      log.push(t + ':' + (e.fontfaces || []).map(function (f) { return f.family + '/' + f.status; }).join(',') + ':' + fs.status + ':' + loadFired + ':' + (e instanceof FontFaceSetLoadEvent) + ':' + e.isTrusted);
    });
  });
  addEventListener('load', function () { loadFired = true; readyLog.push('load:' + fs.status); });
  row('initial', [fs.size, fs.status, fs.ready === fs.ready].join('|'));
  var ready0 = fs.ready;
  ready0.then(function (s) { readyLog.push('ready0:' + (s === fs) + ':' + loadFired); });
  var st = document.createElement('style');
  st.textContent = "@font-face{font-family:'F9 Used';src:url(/f9-used.woff2) format('woff2')}" +
    "@font-face{font-family:F9Unused;font-weight:700;font-style:italic;src:url(/f9-unused.woff2) format('woff2'),local('DejaVu Sans');unicode-range:U+0-7F;font-display:swap}" +
    "@font-face{font-family:F9Local;src:local('DejaVu Sans')}" +
    "@font-face{font-family:F9NoSrc}" +
    "@media print{@font-face{font-family:F9Print;src:url(/f9-print.woff2)}}" +
    "@supports (display:grid){@font-face{font-family:F9Supports;src:url(/f9-supports.woff2)}}" +
    "@font-face{font-family:F9Bad;src:bogus(x)}" +
    "@font-face{font-family:serif;src:url(/f9-generic.woff2)}" +
    "@font-face{font-family:F9Wide;font-weight:300 500;font-stretch:condensed;src:url(/f9-wide.woff2)}" +
    ".u{font-family:'F9 Used',serif}.l{font-family:F9Local}";
  document.head.appendChild(st);
  row('after-style', [fs.size, fs.status, fs.ready === ready0].join('|'));
  row('faces-1', faces());
  var first = Array.from(fs)[0];
  row('identity', first === Array.from(fs)[0]);
  var mine = new FontFace('F9Mine', 'url(/f9-mine.woff2)');
  row('add-returns', fs.add(mine) === fs);
  row('order', Array.from(fs).map(function (f) { return f.family; }).join(',') + '|' + fs.size + '|' + [...fs.keys()].length + '|' + [...fs.entries()].length);
  row('has', [fs.has(first), fs.has(mine), fs.has(new FontFace('F9Mine', 'url(/x.woff2)'))].join('|'));
  row('delete-css', [fs.delete(first), fs.size, fs.has(first)].join('|'));
  row('check', [fs.check('16px "F9 Used"'), fs.check('16px F9Local'), fs.check('bold italic 16px F9Unused'), fs.check('bold italic 16px F9Unused', 'é'),
    fs.check('16px F9NoSrc'), fs.check('16px F9Wide'), fs.check('16px Arial'), fs.check('16px F9Mine'), fs.check('16px serif')].join('|'));
  var d = document.createElement('div'); d.className = 'u'; d.textContent = 'used text'; document.body.appendChild(d);
  var d2 = document.createElement('div'); d2.className = 'l'; d2.textContent = 'local text'; document.body.appendChild(d2);
  // a layout before the load event, so the faces it needs start loading before it (as Chrome's first frame would
  // most often do; this engine's style-induced loads run when the parse ends)
  row('laid-out', d.offsetWidth > 0 && d2.offsetWidth > 0);
  if (document.readyState !== 'complete') await new Promise(function (r) { addEventListener('load', r); });
  await new Promise(function (r) { setTimeout(r, 300); });
  row('after-load', faces() + '|' + fs.status + '|' + log.join(';') + '|' + readyLog.join(';') + '|' + (fs.ready === ready0));
  log.length = 0;
  var unused = Array.from(fs).filter(function (f) { return f.family === 'F9Unused'; })[0];
  var r1 = await fs.load('bold italic 16px F9Unused').then(function (a) { return 'res:' + a.map(function (f) { return f.family + '/' + f.status; }).join(','); }, function (e) { return 'rej:' + e.name; });
  row('set-load-unused', r1 + '|' + (unused && unused.status));
  var r2 = await fs.load('16px F9Wide', '中').then(function (a) { return 'res:' + a.length; }, function (e) { return 'rej:' + e.name; });
  row('set-load-wide', r2 + '|' + faces());
  await new Promise(function (r) { setTimeout(r, 100); });
  row('events-2', log.join(';') + '|' + fs.status);
  row('ready-again', (fs.ready === ready0) + '|' + fs.status);
  st.textContent += "@font-face{font-family:F9Appended;src:local('DejaVu Serif')}";
  row('after-edit', Array.from(fs).map(function (f) { return f.family; }).join(',') + '|' + (Array.from(fs)[0] === first) + '|' + fs.size);
  var st2 = document.createElement('style');
  st2.textContent = "@font-face{font-family:F9Second;src:local('DejaVu Serif')}";
  var before = Array.from(fs)[0];
  document.head.appendChild(st2);
  row('after-append', Array.from(fs).map(function (f) { return f.family; }).join(',') + '|' + (Array.from(fs)[0] === before) + '|' + fs.size);
  st.remove();
  row('after-removal', fs.size + '|' + Array.from(fs).map(function (f) { return f.family; }).join(','));
  return out;
})()
