(function () {
  // ABPROG phase 3, F9: which families LAYOUT sets text in -- the persona's fonts, as its capture records them
  // (plugins/korvus-persona/capture/fonts-probe.html -> fonts/metrics.json). The question detection scripts ask of the
  // DOM (BrowserScan, testpages' fontdetect): does a 72px span of 'mmmmmmmmmmlli' in '<family>, <generic>' lay out
  // unlike the same span in '<generic>' for one of the three generics? The list is BrowserScan's detections on this
  // persona in stock Chrome 150 and the names a sensor cookie and a fingerprinting library ask for.
  var list = ['Arial', 'Helvetica', 'arial', 'Courier', 'Courier New', 'DejaVu Sans Mono', 'MS Gothic', 'MS PGothic', 'Times', 'Times New Roman',
    'Noto Serif', 'Noto Sans', 'Noto Color Emoji', 'PMingLiU', 'SimSun', 'MS PMincho', 'Noto Nastaliq Urdu', 'Noto Sans Adlam', 'Noto Sans Bhaiksuki',
    'Noto Sans Brahmi', 'Noto Sans Chakma', 'Noto Sans Cham', 'Noto Sans Cuneiform', 'Noto Sans Egyptian Hieroglyphs', 'Noto Sans Inscriptional Pahlavi',
    'Noto Sans Inscriptional Parthian', 'Noto Sans Javanese', 'Noto Sans Kaithi', 'Noto Sans Kannada', 'Noto Sans Kharoshthi', 'Noto Sans Khojki',
    'Noto Sans Khudawadi', 'Noto Sans Lepcha', 'Noto Sans Mandaic', 'Noto Sans Manichaean', 'Noto Sans Marchen', 'Noto Sans Meetei Mayek',
    'Noto Sans Meroitic', 'Noto Sans Miao', 'Noto Sans Modi', 'Noto Sans Mongolian', 'Noto Sans Myanmar', 'Noto Sans Newa', 'Noto Sans Oriya',
    'Noto Sans Psalter Pahlavi', 'Noto Sans Sharada', 'Noto Sans Sundanese', 'Noto Sans Syriac', 'Noto Sans Tai Tham', 'Noto Sans Takri',
    'Noto Sans Thaana', 'Noto Sans Tirhuta', 'Noto Sans Ugaritic', 'Noto Serif Ahom', 'Noto Serif Balinese', 'Noto Serif Myanmar', 'Liberation Sans',
    'Ubuntu', 'C059', 'Liberation Mono', 'D050000L', 'DejaVu Sans', 'DejaVu Serif', 'Liberation Sans Narrow', 'Nimbus Mono PS', 'Nimbus Roman',
    'Nimbus Sans', 'Nimbus Sans Narrow', 'Noto Mono', 'Noto Sans CJK HK', 'Noto Sans CJK JP', 'Noto Sans CJK KR', 'Noto Sans CJK SC',
    'Noto Sans CJK TC', 'Noto Sans Mono', 'Noto Sans Mono CJK HK', 'Noto Sans Mono CJK JP', 'Noto Sans Mono CJK KR', 'Noto Sans Mono CJK SC',
    'Noto Sans Mono CJK TC', 'Noto Serif CJK JP', 'Noto Serif CJK KR', 'Noto Serif CJK SC', 'Noto Serif CJK TC', 'P052', 'Standard Symbols PS',
    'Ubuntu Mono', 'URW Bookman', 'URW Gothic', 'Z003', 'MingLiU', 'Bitstream Vera Sans Mono', 'Century', 'Calibri', 'Cambria', 'Verdana',
    'Georgia', 'Tahoma', 'Batang', 'Segoe UI', 'Helvetica Neue', 'Menlo', 'MS Mincho', 'SimHei', 'Arial Unicode MS', 'Lucida Sans'];
  var base = ['monospace', 'sans-serif', 'serif'], s = document.createElement('span');
  s.style.fontSize = '72px'; s.style.whiteSpace = 'pre'; s.textContent = 'mmmmmmmmmmlli';
  var W = {};
  base.forEach(function (b) { s.style.fontFamily = b; document.body.appendChild(s); W[b] = s.offsetWidth; s.remove(); });
  return list.map(function (f) {
    var det = base.filter(function (b) { s.style.fontFamily = '"' + f + '", ' + b; document.body.appendChild(s); var w = s.offsetWidth; s.remove(); return w !== W[b]; });
    return ['layout ' + f, det.join(',') || '-'];
  });
})()
