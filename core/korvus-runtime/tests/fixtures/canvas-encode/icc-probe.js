(function () {
  /* which canvases' exports carry the sRGB ICC profile (a WebP ICCP chunk / a JPEG APP2 ICC_PROFILE): the snapshot of a
     2D context's resource provider carries the canvas colour space; a canvas whose provider was never made exports
     Blink's colour-space-less transparent image. One row per way of touching the canvas. */
  var has = function (url, tag) { var b = atob(url.split(',')[1]); return b.indexOf(tag) >= 0; };
  var cases = {
    'no context': function (c) {},
    'getContext only': function (c) { c.getContext('2d'); },
    'fillStyle only': function (c) { c.getContext('2d').fillStyle = 'red'; },
    'save/restore/translate': function (c) { var g = c.getContext('2d'); g.save(); g.translate(1, 1); g.restore(); },
    'path no fill': function (c) { var g = c.getContext('2d'); g.beginPath(); g.rect(0, 0, 4, 4); },
    'clearRect': function (c) { c.getContext('2d').clearRect(0, 0, 8, 8); },
    'fillRect 0x0': function (c) { c.getContext('2d').fillRect(0, 0, 0, 0); },
    'fillRect outside': function (c) { c.getContext('2d').fillRect(100, 100, 4, 4); },
    'fillRect transparent': function (c) { var g = c.getContext('2d'); g.fillStyle = 'rgba(0,0,0,0)'; g.fillRect(0, 0, 4, 4); },
    'fill empty path': function (c) { var g = c.getContext('2d'); g.beginPath(); g.fill(); },
    'stroke empty path': function (c) { var g = c.getContext('2d'); g.beginPath(); g.stroke(); },
    'fillText empty': function (c) { c.getContext('2d').fillText('', 1, 1); },
    'fillText x': function (c) { c.getContext('2d').fillText('x', 1, 6); },
    'measureText': function (c) { c.getContext('2d').measureText('x'); },
    'getImageData': function (c) { c.getContext('2d').getImageData(0, 0, 2, 2); },
    'putImageData': function (c) { var g = c.getContext('2d'); g.putImageData(g.createImageData(2, 2), 0, 0); },
    'drawImage canvas': function (c) { var s = document.createElement('canvas'); s.width = s.height = 2; c.getContext('2d').drawImage(s, 0, 0); },
    'reset after draw': function (c) { var g = c.getContext('2d'); g.fillRect(0, 0, 2, 2); g.reset(); },
    'width set after draw': function (c) { var g = c.getContext('2d'); g.fillRect(0, 0, 2, 2); c.width = 8; },
    'toDataURL twice': function (c) { var g = c.getContext('2d'); c.toDataURL(); },
    'fillRect then toDataURL png': function (c) { var g = c.getContext('2d'); g.fillRect(0, 0, 2, 2); }
  };
  var out = [];
  for (var k in cases) {
    var c = document.createElement('canvas'); c.width = 8; c.height = 8;
    try { cases[k](c); } catch (e) { out.push([k, 'THROW ' + e]); continue; }
    var w = c.toDataURL('image/webp'), j = c.toDataURL('image/jpeg'), p = c.toDataURL('image/png');
    // (the recording also took the three lengths; the fixture keeps the profile's presence, which no font decides)
    out.push([k, has(w, 'ICCP'), has(j, 'ICC_PROFILE'), has(p, 'iCCP') || has(p, 'sRGB')]);
  }
  return out;
})()
