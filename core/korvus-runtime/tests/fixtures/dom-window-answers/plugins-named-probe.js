(function () {
  // PluginArray / Plugin / MimeTypeArray: indexed properties enumerable, named ones not
  // ([LegacyUnenumerableNamedProperties]); what for-in, keys, JSON and a plugin string make of that.
  var out = [];
  function add(k, f) { try { var v = f(); out.push([k, v === undefined ? 'undefined' : v]); } catch (e) { out.push([k, 'THROW ' + e.name + ': ' + e.message]); } }
  function desc(o, k) { var d = Object.getOwnPropertyDescriptor(o, k); return d ? JSON.stringify({ e: d.enumerable, w: d.writable, c: d.configurable, t: typeof d.value }) : 'none'; }
  var P = navigator.plugins, M = navigator.mimeTypes;
  add('plugins.length', function () { return P.length; });
  add('mimeTypes.length', function () { return M.length; });
  [['plugins', P], ['plugins[0]', P[0]], ['mimeTypes', M]].forEach(function (pair) {
    var n = pair[0], o = pair[1];
    add('for-in ' + n, function () { var k = []; for (var x in o) k.push(x); return k.join(','); });
    add('Object.keys ' + n, function () { return Object.keys(o).join(','); });
    add('own names ' + n, function () { return Object.getOwnPropertyNames(o).join(','); });
    add('JSON ' + n, function () { return JSON.stringify(o); });
    add('entries keys ' + n, function () { return Object.entries(o).map(function (e) { return e[0]; }).join(','); });
  });
  add('descriptor plugins[0] 0', function () { return desc(P[0], '0'); });
  add('descriptor plugins[0] application/pdf', function () { return desc(P[0], 'application/pdf'); });
  add('descriptor plugins 0', function () { return desc(P, '0'); });
  add('descriptor plugins PDF Viewer', function () { return desc(P, 'PDF Viewer'); });
  add('descriptor mimeTypes 0', function () { return desc(M, '0'); });
  add('descriptor mimeTypes text/pdf', function () { return desc(M, 'text/pdf'); });
  add('propertyIsEnumerable plugins PDF Viewer', function () { return P.propertyIsEnumerable('PDF Viewer'); });
  add('in plugins PDF Viewer', function () { return 'PDF Viewer' in P; });
  add('namedItem is index 0', function () { return P.namedItem('PDF Viewer') === P[0]; });
  add('mimeTypes[0] for-in', function () { var k = []; for (var x in M[0]) k.push(x); return k.join(','); });
  add('plugin string (for-in over each plugin)', function () {
    var s = [];
    for (var i = 0; i < P.length; i++) { var p = P[i], m = []; for (var k in p) { if (typeof p[k] === 'object' && p[k] && p[k].type) m.push(p[k].type); } s.push(p.name + '::' + m.join('~')); }
    return s.join(';');
  });
  add('assign named in strict', function () { 'use strict'; try { P[0]['application/pdf'] = 1; return 'no throw'; } catch (e) { return e.name; } });
  add('assign named sloppy keeps', function () { P['PDF Viewer'] = 2; return P['PDF Viewer'] === P[0]; });
  add('define named', function () { try { Object.defineProperty(M, 'text/pdf', { value: 3 }); return M['text/pdf'] === 3; } catch (e) { return 'THROW ' + e.name; } });
  add('delete named', function () { return delete P[0]['text/pdf']; });
  add('after delete named', function () { return typeof P[0]['text/pdf']; });
  return out;
})()
