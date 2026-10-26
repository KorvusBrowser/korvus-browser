(function () {
  // A named child frame is reached through the Window's named properties object, never as an own
  // property of the window (HTML "document-tree child navigable target name property set").
  var rows = [];
  var WP = Object.getPrototypeOf(Window.prototype);
  function d(o, k) { var x = Object.getOwnPropertyDescriptor(o, k); return x ? JSON.stringify({ v: x.value === undefined ? 'undefined' : (x.value === null ? 'null' : typeof x.value), w: x.writable, e: x.enumerable, c: x.configurable, g: typeof x.get }) : 'none'; }
  var before = Object.getOwnPropertyNames(window).length;
  var f = document.createElement('iframe'); f.name = 'fnamedx1'; f.style.display = 'none'; document.body.appendChild(f);
  var own = Object.getOwnPropertyNames(window);
  rows.push(['own names grew by', own.length - before]);
  rows.push(['own names include frame name', own.indexOf('fnamedx1') >= 0]);
  rows.push(['hasOwnProperty(window, name)', Object.prototype.hasOwnProperty.call(window, 'fnamedx1')]);
  rows.push(['descriptor on window', d(window, 'fnamedx1')]);
  rows.push(['descriptor on Window.prototype', d(Window.prototype, 'fnamedx1')]);
  rows.push(['descriptor on WindowProperties', d(WP, 'fnamedx1')]);
  rows.push(['WindowProperties own names', JSON.stringify(Object.getOwnPropertyNames(WP))]);
  rows.push(['name in window', 'fnamedx1' in window]);
  rows.push(['window.name is the frame', window.fnamedx1 === f.contentWindow]);
  rows.push(['frames.name is the frame', frames.fnamedx1 === f.contentWindow]);
  rows.push(['self[name] is the frame', self['fnamedx1'] === f.contentWindow]);
  rows.push(['bare identifier', typeof fnamedx1, (function () { try { return fnamedx1 === f.contentWindow; } catch (e) { return e.name; } })()]);
  rows.push(['Object.keys has it', Object.keys(window).indexOf('fnamedx1') >= 0]);
  var fi = []; for (var k in window) fi.push(k);
  rows.push(['for-in has it', fi.indexOf('fnamedx1') >= 0]);
  rows.push(['delete answers', delete window.fnamedx1, window.fnamedx1 === f.contentWindow]);
  // a window property of the same name shadows the frame
  window.fnamedx3 = 7;
  var f3 = document.createElement('iframe'); f3.name = 'fnamedx3'; document.body.appendChild(f3);
  rows.push(['own property shadows', window.fnamedx3, d(window, 'fnamedx3')]);
  // a name on EventTarget.prototype / Object.prototype is not hidden by a frame
  var f4 = document.createElement('iframe'); f4.name = 'addEventListener'; document.body.appendChild(f4);
  rows.push(['EventTarget member wins', window.addEventListener === EventTarget.prototype.addEventListener]);
  var f5 = document.createElement('iframe'); f5.name = 'toString'; document.body.appendChild(f5);
  rows.push(['Object.prototype member wins', window.toString === Object.prototype.toString]);
  // an element id and a frame name alike: the frame wins
  var dv = document.createElement('div'); dv.id = 'dupname1'; document.body.appendChild(dv);
  var f6 = document.createElement('iframe'); f6.name = 'dupname1'; document.body.appendChild(f6);
  rows.push(['frame before an id', window.dupname1 === f6.contentWindow, window.dupname1 === dv]);
  // two frames of one name: the first in tree order
  var f7 = document.createElement('iframe'); f7.name = 'twice1'; document.body.appendChild(f7);
  var f8 = document.createElement('iframe'); f8.name = 'twice1'; document.body.insertBefore(f8, document.body.firstChild);
  rows.push(['two frames of one name', window.twice1 === f8.contentWindow, window.twice1 === f7.contentWindow]);
  // the browsing context name set from inside
  var f9 = document.createElement('iframe'); document.body.appendChild(f9);
  f9.contentWindow.name = 'innername1';
  rows.push(['name set from inside', window.innername1 === f9.contentWindow, 'innername1' in window]);
  // the name attribute changed after insertion
  var f10 = document.createElement('iframe'); f10.name = 'before1'; document.body.appendChild(f10);
  f10.name = 'after1';
  rows.push(['attribute renamed', window.before1 === f10.contentWindow, window.after1 === f10.contentWindow, f10.contentWindow.name]);
  // a frame in a frame is its own window's, not the page's
  var inner = f9.contentDocument.createElement('iframe'); inner.name = 'nested1'; f9.contentDocument.body.appendChild(inner);
  rows.push(['nested frame', f9.contentWindow.nested1 === inner.contentWindow, window.nested1 === undefined, Object.prototype.hasOwnProperty.call(f9.contentWindow, 'nested1')]);
  // removed
  f.remove();
  rows.push(['removed frame', window.fnamedx1, 'fnamedx1' in window]);
  // a numeric name
  var f11 = document.createElement('iframe'); f11.name = '123'; document.body.appendChild(f11);
  rows.push(['numeric name', window['123'] === f11.contentWindow, '123' in window]);
  rows.push(['own names (final, filtered)', JSON.stringify(Object.getOwnPropertyNames(window).filter(function (n) { return /named|dupname|twice|innername|before1|after1|nested|^\d+$/.test(n); }))]);
  return rows;
})()
