(function () {
  // chrome.loadTimes / chrome.csi are the v8/LoadTimes extension's plain function expressions
  // (chrome/renderer/loadtimes_extension_bindings.cc): their own properties, prototype and construction.
  var rows = [];
  function d(o, k) { var x = Object.getOwnPropertyDescriptor(o, k); return x ? JSON.stringify({ t: typeof x.value, w: x.writable, e: x.enumerable, c: x.configurable }) : 'none'; }
  function add(label, f) { try { rows.push([label, f()]); } catch (e) { rows.push([label, 'THROW ' + e.name + ': ' + e.message]); } }
  function shape(w, tag) {
    var c = w.chrome;
    ['loadTimes', 'csi'].forEach(function (n) {
      var fn = c[n];
      add(tag + n + ' own names', function () { return Object.getOwnPropertyNames(fn).join(','); });
      add(tag + n + ' name/length', function () { return JSON.stringify(fn.name) + '/' + fn.length; });
      add(tag + n + ' prototype desc', function () { return d(fn, 'prototype'); });
      add(tag + n + ' prototype own names', function () { return Object.getOwnPropertyNames(fn.prototype).join(','); });
      add(tag + n + ' prototype.constructor desc', function () { return d(fn.prototype, 'constructor'); });
      add(tag + n + ' prototype.constructor is fn', function () { return fn.prototype.constructor === fn; });
      add(tag + n + ' prototype proto is Object.prototype', function () { return Object.getPrototypeOf(fn.prototype) === w.Object.prototype; });
      add(tag + n + ' proto is Function.prototype', function () { return Object.getPrototypeOf(fn) === w.Function.prototype; });
      add(tag + n + ' toString', function () { return w.Function.prototype.toString.call(fn); });
      add(tag + 'new ' + n + '()', function () { var o = new fn(); return typeof o + ' ' + Object.keys(o).join(',') + ' instanceof:' + (o instanceof fn); });
      add(tag + 'Reflect.construct ' + n, function () { var o = Reflect.construct(fn, [], Object); return Object.keys(o).length + ' ' + (Object.getPrototypeOf(o) === Object.prototype); });
      add(tag + n + '.call(1) keys', function () { return Object.keys(fn.call(1)).join(','); });
      add(tag + n + ' prototype same object', function () { return fn.prototype === fn.prototype; });
      add(tag + n + ' isExtensible', function () { return Object.isExtensible(fn); });
    });
    ['getDetails', 'getIsInstalled', 'installState', 'runningState'].forEach(function (n) {
      var fn = c.app[n];
      add(tag + 'app.' + n + ' own names', function () { return Object.getOwnPropertyNames(fn).join(','); });
      add(tag + 'new app.' + n + '()', function () { var o = new fn(); return typeof o; });
      add(tag + 'app.' + n + ' toString', function () { return w.Function.prototype.toString.call(fn); });
    });
  }
  shape(window, '');
  var f = document.createElement('iframe'); document.body.appendChild(f);
  shape(f.contentWindow, 'frame ');
  add('frame loadTimes distinct', function () { return f.contentWindow.chrome.loadTimes !== chrome.loadTimes; });
  add('frame new csi realm', function () { var o = new f.contentWindow.chrome.csi(); return Object.getPrototypeOf(o) === f.contentWindow.Object.prototype; });
  return rows;
})()
