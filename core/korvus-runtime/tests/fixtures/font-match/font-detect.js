(function () {
  // ABPROG phase 3, F9: which named families canvas text is set in, as Chrome 150's font service matches them on Linux
  // (Skia's SkFontConfigInterfaceDirect::matchFamilyName: fontconfig's answer, kept only when it is the family or a
  // metric-compatible one; then Blink's alternate name). a sensor cookie font probe's question: does
  // '72px "<family>", <generic>' measure unlike '72px <generic>' for each of the three generics?
  var c = document.createElement('canvas'), x = c.getContext('2d'), txt = 'mmmmmmmmmmlli';
  function w(font) { x.font = font; return x.measureText(txt).width; }
  var out = [], gens = ['monospace', 'sans-serif', 'serif'], base = {}, bold = {};
  gens.forEach(function (g) { base[g] = w('72px ' + g); bold[g] = w('bold 72px ' + g); });
  ['PMingLiU', 'MingLiU', 'PMingLiU_HKSCS', 'BitstreamVeraSansMono', 'Bitstream Vera Sans Mono', 'Bitstream Vera Sans', 'Century', 'CenturyGothic',
   'Century Schoolbook', 'Calibri', 'Cambria', 'Carlito', 'Caladea', 'Arimo', 'Tinos', 'Cousine', 'Symbol', 'Arial', 'Times New Roman', 'Courier New',
   'Liberation Sans', 'Liberation Serif', 'Liberation Mono', 'DejaVu Sans', 'DejaVu Sans Mono', 'DejaVu Serif', 'Noto Serif CJK TC', 'Noto Sans CJK JP',
   'Helvetica', 'Times', 'Courier', 'Helvetica Neue', 'Palatino', 'Bookman', 'Ubuntu', 'Ubuntu Mono', 'Verdana', 'Georgia', 'Tahoma', 'Batang',
   'MS Gothic', 'MS PGothic', 'MS Mincho', 'MS PMincho', 'SimSun', 'NSimSun', 'SimHei', 'Segoe UI', 'Nimbus Sans', 'C059', 'URW Gothic',
   'Univers CE 55 Medium', 'sans', 'Monospace', 'serif'].forEach(function (f) {
    var det = gens.filter(function (g) { return w('72px "' + f + '", ' + g) !== base[g]; });
    var detBold = gens.filter(function (g) { return w('bold 72px "' + f + '", ' + g) !== bold[g]; });
    out.push(['detected ' + f, det.join(',') || '-', detBold.join(',') || '-']);
  });
  return out;
})()
