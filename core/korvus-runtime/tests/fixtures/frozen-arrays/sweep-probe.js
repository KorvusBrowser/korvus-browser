(function () {
  // Every attribute Blink 150's IDL declares `FrozenArray<T>` that a page reaches without a permission or a device, in
  // a window and in a dedicated worker: [label, kind, Object.isFrozen, length's [[Writable]], the same object on a
  // second read, length, then entry 0's [writable, enumerable, configurable] and what entry 0 is (a primitive's typeof,
  // or an object read twice: the same one or a new one, and frozen or not). The `identity` rows walk a
  // [CachedAttribute] through the changes that make it dirty; `composedPath()` is the control -- a sequence, not a
  // FrozenArray, and not frozen.
  function shapeOf(label, get) {
    function d(o, k) { var x = Object.getOwnPropertyDescriptor(o, k); return x ? [x.writable, x.enumerable, x.configurable] : 'none'; }
    var a, b;
    try { a = get(); b = get(); } catch (e) { return [label, 'throw', e.name]; }
    if (a === null || typeof a !== 'object') return [label, 'value', a === null ? null : typeof a];
    var r = [label, Array.isArray(a) ? 'array' : Object.prototype.toString.call(a), Object.isFrozen(a), d(a, 'length')[0], a === b, a.length];
    if (a.length > 0) {
      r.push(d(a, 0));
      var e = a[0];
      r.push(e !== null && typeof e === 'object' ? (e === b[0] ? 'same entry' : 'new entry') + (Object.isFrozen(e) ? ', frozen' : '') : typeof e);
    }
    return r;
  }
  function attempt(label, f) { try { return f(); } catch (e) { return [label, 'throw', e.name, String(e.message)]; } }
  var rows = [];
  var shape = function (label, get) { rows.push(shapeOf(label, get)); };

  // ── navigator ──────────────────────────────────────────────────────────────────────────────────────────────────
  shape('Navigator.languages', function () { return navigator.languages; });
  rows.push(attempt('Navigator.languages push', function () {
    var l = navigator.languages, n = l.length, w;
    try { l.push('x'); w = 'pushed'; } catch (e) { w = e.name; }
    return ['Navigator.languages push', w, navigator.languages.length === n];
  }));
  rows.push(attempt('Navigator.languages after a synthetic languagechange', function () {
    var l0 = navigator.languages;
    dispatchEvent(new Event('languagechange'));
    return ['Navigator.languages after a synthetic languagechange', l0 === navigator.languages];
  }));
  shape('NavigatorUAData.brands', function () { return navigator.userAgentData.brands; });

  // ── DataTransfer.types: [CachedAttribute=hasDataStoreItemListChanged] ───────────────────────────────────────────
  var dt = new DataTransfer();
  shape('DataTransfer.types (empty)', function () { return dt.types; });
  rows.push(attempt('DataTransfer.types identity', function () {
    var t0 = dt.types;
    dt.setData('text/plain', 'x'); var t1 = dt.types, t1b = dt.types;
    dt.setData('text/plain', 'y'); var t2 = dt.types;
    dt.clearData('text/html'); var t3 = dt.types;
    dt.clearData('text/plain'); var t4 = dt.types;
    dt.items.add('z', 'text/html'); var t5 = dt.types;
    dt.items.remove(5); var t6 = dt.types;
    dt.items.remove(0); var t7 = dt.types;
    dt.items.clear(); var t8 = dt.types;
    return ['DataTransfer.types identity', t1 === t0, t1 === t1b, t2 === t1, t3 === t2, t4 === t3, t5 === t4, t6 === t5, t7 === t6, t8 === t7,
      JSON.stringify([t0, t1, t2, t3, t4, t5, t6, t7, t8])];
  }));
  dt.setData('text/uri-list', 'http://a.test/');
  shape('DataTransfer.types (one type)', function () { return dt.types; });

  // ── ClipboardItem: the constructor's record, and `types` ───────────────────────────────────────────────────────
  rows.push(attempt('ClipboardItem.types', function () {
    var ci = new ClipboardItem({ 'text/plain': new Blob(['x'], { type: 'text/plain' }), 'text/html': '<b>x</b>' });
    return shapeOf('ClipboardItem.types', function () { return ci.types; });
  }));
  [['5', function () { return 5; }], ['{}', function () { return {}; }], ['symbol key', function () { var o = { 'text/plain': 'a' }; o[Symbol('s')] = 'x'; return o; }],
   ['getter throws', function () { return { get 'text/plain'() { throw new RangeError('boom'); } }; }]].forEach(function (c) {
    rows.push(attempt('new ClipboardItem(' + c[0] + ')', function () { new ClipboardItem(c[1]()); return ['new ClipboardItem(' + c[0] + ')', 'ok']; }));
  });
  rows.push(attempt('ClipboardItem.types values', function () {
    var o = { 'z': 'a', '2': 'b', '1': 'c', 'web text/x': 'd', 'web  text/y ': 'e', 'web  bad': 'f', 'web x/y; q=1': 'g', 'TEXT/HTML': 'h' };
    Object.defineProperty(o, 'hidden', { value: 'i', enumerable: false });
    return ['ClipboardItem.types values', new ClipboardItem(o).types.join('|')];
  }));
  rows.push(attempt('ClipboardItem reads', function () {
    var log = [];
    var p = new Proxy({ 'text/plain': 'a', 'text/html': 'b' }, {
      ownKeys: function (o) { log.push('ownKeys'); return Reflect.ownKeys(o); },
      getOwnPropertyDescriptor: function (o, k) { log.push('gopd:' + String(k)); return Reflect.getOwnPropertyDescriptor(o, k); },
      get: function (o, k) { log.push('get:' + String(k)); return Reflect.get(o, k); }
    });
    new ClipboardItem(p);
    return ['ClipboardItem reads', log.join(',')];
  }));
  rows.push(attempt('ClipboardItem.types foreign receiver', function () {
    var g = Object.getOwnPropertyDescriptor(ClipboardItem.prototype, 'types').get;
    try { g.call(Object.create(ClipboardItem.prototype)); return ['ClipboardItem.types foreign receiver', 'no throw']; } catch (e) { return ['ClipboardItem.types foreign receiver', e.name, e.message]; }
  }));

  // ── MediaMetadata / ChapterInformation ─────────────────────────────────────────────────────────────────────────
  var mm;
  rows.push(attempt('MediaMetadata.artwork', function () {
    mm = new MediaMetadata({ title: 't', artwork: [{ src: '/a.png', sizes: '1x1', type: 'image/png' }], chapterInfo: [{ title: 'c', startTime: 1, artwork: [{ src: '/c.png' }] }] });
    return shapeOf('MediaMetadata.artwork', function () { return mm.artwork; });
  }));
  rows.push(attempt('MediaImage members', function () {
    return ['MediaImage members', Object.keys(mm.artwork[0]).join(','), Object.keys(mm.chapterInfo[0].artwork[0]).join(',')];
  }));
  shape('MediaMetadata.chapterInfo', function () { return mm.chapterInfo; });
  shape('ChapterInformation.artwork', function () { return mm.chapterInfo[0].artwork; });
  rows.push(attempt('MediaMetadata.artwork (set)', function () {
    mm.artwork = [{ src: '/b.png' }, { src: '/b2.png' }];
    return shapeOf('MediaMetadata.artwork (set)', function () { return mm.artwork; });
  }));

  // ── constructed objects ───────────────────────────────────────────────────────────────────────────────────────
  rows.push(attempt('IntersectionObserver.thresholds', function () {
    var io = new IntersectionObserver(function () {}, { threshold: [0.5, 0, 1] });
    return shapeOf('IntersectionObserver.thresholds', function () { return io.thresholds; });
  }));
  rows.push(attempt('IntersectionObserver.thresholds values', function () {
    var io = new IntersectionObserver(function () {}, { threshold: [0.1, 1 + 1e-12, 0.3] });
    return ['IntersectionObserver.thresholds values', io.thresholds.join(',')];
  }));
  rows.push(attempt('FontFaceSetLoadEvent.fontfaces', function () {
    var fe = new FontFaceSetLoadEvent('loading', { fontfaces: [new FontFace('fa', 'url(/x.woff)')] });
    return shapeOf('FontFaceSetLoadEvent.fontfaces', function () { return fe.fontfaces; });
  }));
  rows.push(attempt('BeforeInstallPromptEvent.platforms', function () {
    var be = new BeforeInstallPromptEvent('beforeinstallprompt', { platforms: ['web', 'play'] });
    return shapeOf('BeforeInstallPromptEvent.platforms', function () { return be.platforms; });
  }));
  var ck;
  rows.push(attempt('CookieChangeEvent.changed', function () {
    ck = new CookieChangeEvent('change', { changed: [{ name: 'a', value: '1' }], deleted: [{ name: 'b' }] });
    return shapeOf('CookieChangeEvent.changed', function () { return ck.changed; });
  }));
  shape('CookieChangeEvent.deleted', function () { return ck.deleted; });
  rows.push(attempt('CookieChangeEvent.changed (none)', function () {
    var ck0 = new CookieChangeEvent('change');
    return shapeOf('CookieChangeEvent.changed (none)', function () { return ck0.changed; });
  }));
  rows.push(attempt('RTCTrackEvent.streams', function () {
    var pc = new RTCPeerConnection(), tr = pc.addTransceiver('audio');
    var te = new RTCTrackEvent('track', { receiver: tr.receiver, track: tr.receiver.track, transceiver: tr, streams: [new MediaStream()] });
    var r = shapeOf('RTCTrackEvent.streams', function () { return te.streams; });
    pc.close();
    return r;
  }));
  var nt;
  rows.push(attempt('Notification.vibrate', function () {
    nt = new Notification('t', { tag: 'frozen-arrays' });
    return shapeOf('Notification.vibrate', function () { return nt.vibrate; });
  }));
  shape('Notification.actions', function () { return nt.actions; });
  rows.push(attempt('USBIsochronousInTransferResult.packets', function () {
    var ur = new USBIsochronousInTransferResult([new USBIsochronousInTransferPacket('ok')]);
    return shapeOf('USBIsochronousInTransferResult.packets', function () { return ur.packets; });
  }));
  rows.push(attempt('USBIsochronousOutTransferResult.packets', function () {
    var uo = new USBIsochronousOutTransferResult([new USBIsochronousOutTransferPacket('ok', 1)]);
    return shapeOf('USBIsochronousOutTransferResult.packets', function () { return uo.packets; });
  }));
  rows.push(attempt('HTMLInputElement.webkitEntries', function () {
    var inp = document.createElement('input');
    inp.type = 'file';
    return shapeOf('HTMLInputElement.webkitEntries', function () { return inp.webkitEntries; });
  }));
  rows.push(attempt('OrientationSensor.quaternion', function () {
    var os = new AbsoluteOrientationSensor();
    return shapeOf('OrientationSensor.quaternion', function () { return os.quaternion; });
  }));
  shape('Viewport.segments', function () { return viewport.segments; });

  // ── statics ([SaveSameObject] on the interface object) ─────────────────────────────────────────────────────────
  shape('PerformanceObserver.supportedEntryTypes', function () { return PerformanceObserver.supportedEntryTypes; });
  shape('PushManager.supportedContentEncodings', function () { return PushManager.supportedContentEncodings; });
  shape('PressureObserver.knownSources', function () { return PressureObserver.knownSources; });

  // ── timing ─────────────────────────────────────────────────────────────────────────────────────────────────────
  shape('PerformanceNavigationTiming.serverTiming', function () { return performance.getEntriesByType('navigation')[0].serverTiming; });

  // ── MessageEvent.ports: [CachedAttribute=isPortsDirty] ──────────────────────────────────────────────────────────
  var ch = new MessageChannel();
  var me = new MessageEvent('message', { ports: [ch.port1] });
  shape('MessageEvent.ports (init)', function () { return me.ports; });
  var me0 = new MessageEvent('message');
  shape('MessageEvent.ports (none)', function () { return me0.ports; });
  rows.push(attempt('MessageEvent.ports identity', function () {
    var p0 = me.ports;
    me.initMessageEvent('message', false, false, null, '', '', null, [ch.port2]);
    var p1 = me.ports, p2 = me.ports;
    return ['MessageEvent.ports identity', p1 === p0, p1 === p2, p1[0] === ch.port2, Object.isFrozen(p1)];
  }));

  // ── reflected element arrays (HTML's "cached attr-associated elements") ───────────────────────────────────────
  var host = document.createElement('div'), ea = document.createElement('i'), eb = document.createElement('b');
  ea.id = 'fa-a';
  eb.id = 'fa-b';
  document.body.append(host, ea, eb);
  shape('Element.ariaControlsElements (unset)', function () { return host.ariaControlsElements; });
  rows.push(attempt('Element.ariaControlsElements', function () {
    host.ariaControlsElements = [ea, eb];
    return shapeOf('Element.ariaControlsElements', function () { return host.ariaControlsElements; });
  }));
  rows.push(attempt('Element.ariaControlsElements identity', function () {
    var x1 = host.ariaControlsElements;
    host.ariaControlsElements = [ea, eb]; var x2 = host.ariaControlsElements;
    host.ariaControlsElements = [eb]; var x3 = host.ariaControlsElements;
    host.ariaControlsElements = null; var x4 = host.ariaControlsElements;
    return ['Element.ariaControlsElements identity', x2 === x1, x3 === x2, x3.length, x4];
  }));
  rows.push(attempt('ElementInternals.ariaDescribedByElements', function () {
    customElements.define('fa-x', class extends HTMLElement { constructor() { super(); this.fi = this.attachInternals(); } });
    var cx = document.createElement('fa-x');
    var given = [ea];
    cx.fi.ariaDescribedByElements = given;
    var r = shapeOf('ElementInternals.ariaDescribedByElements', function () { return cx.fi.ariaDescribedByElements; });
    r.push(cx.fi.ariaDescribedByElements === given);
    var a1 = cx.fi.ariaDescribedByElements;
    cx.fi.ariaDescribedByElements = [ea];
    r.push(cx.fi.ariaDescribedByElements === a1);
    return r;
  }));

  // ── CSSOM ──────────────────────────────────────────────────────────────────────────────────────────────────────
  var st = document.createElement('style');
  st.textContent = '@layer fa1, fa2; @container fc (min-width: 1px) { p { color: red } } @container (min-height: 2px) { p { color: blue } }';
  document.head.appendChild(st);
  function rules(name) {
    var C = self[name];
    return Array.prototype.filter.call(st.sheet.cssRules, function (r) { return r instanceof C; });
  }
  shape('CSSLayerStatementRule.nameList', function () { return rules('CSSLayerStatementRule')[0].nameList; });
  shape('CSSContainerRule.conditions', function () { return rules('CSSContainerRule')[0].conditions; });
  rows.push(attempt('CSSContainerRule.conditions values', function () {
    return ['CSSContainerRule.conditions values', JSON.stringify(rules('CSSContainerRule').map(function (r) { return r.conditions; }))];
  }));

  // ── the control: a method's sequence is not a FrozenArray ─────────────────────────────────────────────────────
  var ev = new Event('x');
  shape('Event.composedPath() (control)', function () { return ev.composedPath(); });

  // ── asynchronous: a delivered message, a resize observation, a dedicated worker ────────────────────────────────
  function delivered() {
    return new Promise(function (res) {
      var mc = new MessageChannel();
      mc.port2.onmessage = function (e) {
        res(shapeOf('MessageEvent.ports (delivered)', function () { return e.ports; }));
      };
      var c2 = new MessageChannel();
      mc.port1.postMessage('x', [c2.port1]);
      setTimeout(function () { res(['MessageEvent.ports (delivered)', 'TIMEOUT']); }, 3000);
    });
  }
  function resized() {
    return new Promise(function (res) {
      var box = document.createElement('div');
      box.style.cssText = 'width: 50px; height: 20px';
      document.body.appendChild(box);
      var ro = new ResizeObserver(function (entries) {
        var en = entries[0];
        ro.disconnect();
        res([shapeOf('ResizeObserverEntry.contentBoxSize', function () { return en.contentBoxSize; }),
             shapeOf('ResizeObserverEntry.borderBoxSize', function () { return en.borderBoxSize; }),
             shapeOf('ResizeObserverEntry.devicePixelContentBoxSize', function () { return en.devicePixelContentBoxSize; })]);
      });
      ro.observe(box);
      setTimeout(function () { res([['ResizeObserverEntry', 'TIMEOUT']]); }, 3000);
    });
  }
  function worker() {
    var src = shapeOf.toString() + '\n' + attempt.toString() + '\n(' + function () {
      var rows = [];
      var shape = function (label, get) { rows.push(shapeOf(label, get)); };
      shape('WorkerNavigator.languages', function () { return navigator.languages; });
      rows.push(attempt('WorkerNavigator.languages push', function () {
        var l = navigator.languages, n = l.length, w;
        try { l.push('x'); w = 'pushed'; } catch (e) { w = e.name; }
        return ['WorkerNavigator.languages push', w, navigator.languages.length === n];
      }));
      shape('NavigatorUAData.brands', function () { return navigator.userAgentData.brands; });
      var me0 = new MessageEvent('message');
      shape('MessageEvent.ports (none)', function () { return me0.ports; });
      rows.push(attempt('MessageEvent.ports (init)', function () {
        var ch = new MessageChannel(), me = new MessageEvent('message', { ports: [ch.port1] });
        return shapeOf('MessageEvent.ports (init)', function () { return me.ports; });
      }));
      shape('PerformanceObserver.supportedEntryTypes', function () { return PerformanceObserver.supportedEntryTypes; });
      shape('PushManager.supportedContentEncodings', function () { return PushManager.supportedContentEncodings; });
      shape('PressureObserver.knownSources', function () { return PressureObserver.knownSources; });
      rows.push(attempt('Notification.vibrate', function () {
        var n = new Notification('t', { tag: 'frozen-arrays' });
        return shapeOf('Notification.vibrate', function () { return n.vibrate; });
      }));
      rows.push(attempt('USBIsochronousInTransferResult.packets', function () {
        var ur = new USBIsochronousInTransferResult([new USBIsochronousInTransferPacket('ok')]);
        return shapeOf('USBIsochronousInTransferResult.packets', function () { return ur.packets; });
      }));
      postMessage(rows);
    } + ')();';
    return new Promise(function (res) {
      var w = new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
      w.onmessage = function (e) { res(e.data.map(function (r) { return ['worker'].concat(r); })); };
      w.onerror = function (e) { res([['worker', 'error', String(e.message)]]); };
      setTimeout(function () { res([['worker', 'TIMEOUT']]); }, 4000);
    });
  }
  return delivered().then(function (r) {
    rows.push(r);
    return resized();
  }).then(function (rs) {
    rows = rows.concat(rs);
    return worker();
  }).then(function (ws) {
    return rows.map(function (r) { return ['window'].concat(r); }).concat(ws);
  });
})()
