(async function () {
  // F9: when document.fonts.ready settles -- the page's before and after its load event, a created document's, a
  // same-origin frame's -- and the order of a local face's load against its `loaded` promise and the set's events.
  var out = [], order = [];
  function row(k, v) { out.push([k, v]); }
  var fs = document.fonts, ready0 = fs.ready, settled0 = 'pending';
  ready0.then(function () { settled0 = 'resolved'; order.push('ready0'); });
  addEventListener('load', function () { order.push('load'); });
  await new Promise(function (r) { setTimeout(r, 0); });
  // (not the readyState: whether the load event has come yet is the page lifecycle's timing, not the font set's)
  row('page-before-load', settled0);
  var other = document.implementation.createHTMLDocument('x'), otherSettled = 'pending';
  other.fonts.ready.then(function () { otherSettled = 'resolved'; });
  var xml = document.implementation.createDocument(null, 'r', null), xmlSettled = 'pending';
  xml.fonts.ready.then(function () { xmlSettled = 'resolved'; });
  var ifr = document.createElement('iframe'); document.body.appendChild(ifr);
  var fdoc = ifr.contentDocument, frameSettled = 'pending';
  fdoc.fonts.ready.then(function () { frameSettled = 'resolved'; });
  if (document.readyState !== 'complete') await new Promise(function (r) { addEventListener('load', r); });
  await new Promise(function (r) { setTimeout(r, 200); });
  row('after-load', [settled0, otherSettled, xmlSettled, frameSettled, fs.ready === ready0, other.fonts.status, fdoc.fonts.status].join('|'));
  row('order', order.join(','));
  // a face added to the set and loaded from a local source: the order of its status, its promise and the events
  var log = [];
  ['loading', 'loadingdone', 'loadingerror'].forEach(function (t) { fs.addEventListener(t, function (e) { log.push(t + ':' + e.fontfaces.length + ':' + fs.status); }); });
  var f = new FontFace('F9R', 'local("DejaVu Sans")');
  fs.add(f);
  var p = f.loaded;
  p.then(function () { log.push('loaded-promise:' + fs.status); });
  var lp = f.load();
  log.push('after-load():' + f.status + ':' + fs.status + ':' + (lp === p));
  var rd = fs.ready;
  rd.then(function () { log.push('ready:' + fs.status); });
  Promise.resolve().then(function () { log.push('microtask:' + fs.status); });
  setTimeout(function () { log.push('task:' + fs.status); }, 0);
  await new Promise(function (r) { setTimeout(r, 100); });
  row('local-load', log.join(';') + '|' + (fs.ready === rd) + '|' + (rd === ready0));
  // a constructed face never added to the set: no set events
  log.length = 0;
  var g = new FontFace('F9S', 'local("DejaVu Sans")');
  g.load();
  await new Promise(function (r) { setTimeout(r, 50); });
  row('unadded-load', log.join(';') + '|' + g.status + '|' + fs.status);
  // fonts.load() for a family of the set's: resolves with the faces, an empty array for a family it lacks
  var r1 = await fs.load('16px F9R').then(function (a) { return a.length + ':' + (a[0] === f); });
  var r2 = await fs.load('16px Nope, serif').then(function (a) { return a.length; });
  row('set-load', r1 + '|' + r2 + '|' + log.join(';'));
  return out;
})()
