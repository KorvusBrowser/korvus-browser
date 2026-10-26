(function () {
  // ABPROG phase 3, F9: canvas text whose words are shaped in different scripts. Blink shapes 8-bit (Latin-1) text
  // as Latin throughout and 16-bit text by its script runs, where a word of only Common characters -- a space --
  // stays Common: under `ko`, Noto Sans CJK KR's space is 0.28 em in `a b` (the Latin `locl` form) and 0.224 em in
  // `😀 漢字 ok`. Each measurement and each drawing is on a FRESH canvas: a canvas keeps the first shaping of a word
  // for the rest of the frame (`FrameShapeCache`), so a string measured after another would answer with its words.
  function m(font, t) { var x = document.createElement('canvas').getContext('2d'); x.font = font; return x.measureText(t).width; }
  function img(font, text, x0) {
    var c = document.createElement('canvas'); c.width = 220; c.height = 60;
    var x = c.getContext('2d'); x.font = font; x.fillStyle = '#000'; x.fillText(text, x0, 36);
    return x.getImageData(0, 0, 220, 60).data;
  }
  function fnv(a) { var h = 2166136261; for (var i = 3; i < a.length; i += 4) { h ^= a[i]; h = Math.imul(h, 16777619) >>> 0; } return ('0000000' + h.toString(16)).slice(-8); }
  function same(a, b, c0, c1) {
    for (var y = 0; y < 60; y++) for (var cx = c0; cx < c1; cx++) { var i = (y * 220 + cx) * 4 + 3; if (a[i] !== b[i]) return false; }
    return true;
  }
  // where fillText puts `glyph` inside `text` drawn at x = 4: the 1/16 px pen positions whose lone glyph matches
  function pen(font, text, glyph, c0, c1, lo, hi) {
    var whole = img(font, text, 4), hits = [];
    for (var p = lo * 16; p <= hi * 16; p++) { if (same(whole, img(font, glyph, p / 16), c0, c1)) hits.push(p / 16); }
    return hits.length ? hits[0] + '..' + hits[hits.length - 1] : 'none';
  }
  var out = [];
  // `"Noto Color Emoji"` named: fontconfig does not anti-alias it, so its strike is fully hinted and advances by
  // whole pixels (no subpixel positioning) -- where the same font as a FALLBACK does not
  ['24px sans-serif', '24px serif', '24px "Noto Sans CJK KR"', '24px Arial', '11pt no-real-font-123', '24px "Noto Color Emoji"'].forEach(function (f) {
    [' ', '\u{1F600} ', '\u{1F600} 漢', '漢字 ok', 'x 漢', 'a b', 'a, b!', '漢, b!', '(漢)', '\u{1F600} 漢字 ok',
     'Cwm fjordbank glyphs vext quiz, \u{1F603}', '가 나', 'é è', 'tab\there'].forEach(function (t) {
      out.push(['width ' + f + ' | ' + JSON.stringify(t), m(f, t)]);
    });
  });
  var F = '24px sans-serif';
  out.push(['pen x in "\u{1F600} x"', pen(F, '\u{1F600} x', 'x', 36, 60, 36, 44)]);
  out.push(['pen x in "漢 x"', pen(F, '漢 x', 'x', 30, 50, 30, 38)]);
  out.push(['pen 漢 in "x 漢"', pen(F, 'x 漢', '漢', 20, 50, 18, 26)]);
  out.push(['pen b in "a b"', pen(F, 'a b', 'b', 20, 45, 18, 26)]);
  ['24px sans-serif', '24px serif', '11pt no-real-font-123'].forEach(function (f) {
    ['\u{1F600} 漢字 ok', 'Cwm fjordbank glyphs vext quiz, \u{1F603}'].forEach(function (t) {
      out.push(['alpha ' + f + ' | ' + JSON.stringify(t), fnv(img(f, t, 4))]);
    });
  });
  return out;
})()
