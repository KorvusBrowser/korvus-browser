(function () {
  /* canPlayType + MediaSource.isTypeSupported over Chromium's MimeUtil containers x codec spellings (no HEVC: that one
     is the device's GPU). One row per container: [container, base canPlayType, base MSE, canPlayType per codec as
     p/m/- , MSE per codec as T/F]; then the spelling rows [string, canPlayType, MSE]. */
  var v = document.createElement('video');
  var cp = function (t) { var r = v.canPlayType(t); return r === 'probably' ? 'p' : r === 'maybe' ? 'm' : r === '' ? '-' : '?' + r; };
  var ms = function (t) { return MediaSource.isTypeSupported(t) ? 'T' : 'F'; };
  var containers = ['audio/wav', 'audio/x-wav', 'audio/webm', 'video/webm', 'audio/ogg', 'video/ogg', 'application/ogg',
    'audio/flac', 'audio/mpeg', 'audio/mp3', 'audio/x-mp3', 'audio/mp4', 'video/mp4', 'audio/matroska', 'video/matroska',
    'audio/x-matroska', 'video/x-matroska', 'audio/aac', 'audio/x-m4a', 'video/x-m4v', 'video/3gpp',
    'application/x-mpegurl', 'application/vnd.apple.mpegurl', 'audio/mpegurl', 'audio/x-mpegurl', 'audio/3gpp',
    'video/mp2t', 'video/quicktime', 'audio/x-aac', 'audio/x-flac', 'audio/wave', 'application/octet-stream', 'video/x-flv'];
  var codecs = ['1', 'mp3', 'mp4a.40.2', 'mp4a.40.02', 'mp4a.40.5', 'mp4a.40.05', 'mp4a.40.29', 'mp4a.40', 'mp4a.40.42',
    'mp4a.40.34', 'mp4a.66', 'mp4a.67', 'mp4a.68', 'mp4a.69', 'mp4a.6B', 'mp4a.6b', 'ac-3', 'ec-3', 'mp4a.a5', 'mp4a.A6',
    'dtsc', 'ac-4', 'vorbis', 'opus', 'Opus', 'OPUS', 'flac', 'fLaC', 'vp8', 'vp8.0', 'vp9', 'vp9.0', 'vp09.00.10.08',
    'vp09.01.20.08.02', 'vp09.02.10.10.01.09.16.09.01', 'vp09.02.10.12', 'vp09.00.10.08.01.01.01.01.00', 'vp09.00.15.08',
    'vp09.00.10.08.03', 'vp09.00.10.08.01.03', 'theora', 'avc1', 'avc3', 'avc1.42E01E', 'avc1.4D401F', 'avc1.64001F',
    'avc1.6E001F', 'avc1.7A001F', 'avc1.F4001F', 'avc1.53001F', 'avc1.58A01E', 'avc1.58001E', 'avc1.42E02B',
    'avc1.42E000', 'avc1.0x42E0', 'avc1.42E11E', 'avc3.640028', 'avc1.64E01F', 'dvh1.05.06', 'dva1.09.05', 'dav1.10.01',
    'av01.0.04M.08', 'av01.0.04M.10.0.112.09.16.09.0', 'av01.1.04M.08', 'av01.2.04M.12', 'av01.0.04X.08', 'av01.0.4M.08',
    'mhm1.0x0D', 'iamf.000.000.Opus', 'av1', 'pcm', ''];
  var out = [];
  containers.forEach(function (c) {
    var a = '', b = '';
    codecs.forEach(function (k) { var t = c + '; codecs="' + k + '"'; a += cp(t); b += ms(t); });
    out.push([c, cp(c), ms(c), a, b]);
  });
  ['', ' ', 'video/mp4;codecs=', 'video/webm; codecs="vp8,,opus"', 'video/webm; codecs="vp8, opus"', 'video/webm; CODECS="vp8"',
   'video/webm; codecs=vp8,opus', 'video/webm; codecs="vp8"; codecs="opus"', 'VIDEO/MP4', ' video/mp4 ', 'video/mp4 ;codecs="avc1.42E01E"',
   'video/mp4; codecs="avc1.42E01E, mp4a.40.2"', 'video/mp4; codecs="avc1, mp4a.40.2"', 'video/mp4; codecs="avc1.42E01E, mp4a.40"',
   'video/mp4; codecs="avc1.6E001F, mp4a.40.2"', 'video/webm; codecs="vp9, opus"', 'video/webm; codecs="vp09.00.10.08, vorbis"',
   'audio/mpeg; codecs="mp3"', 'audio/mpeg; codecs="mp4a.40.2"', 'audio/flac; codecs="flac"', 'audio/aac; codecs="mp4a.40.2"',
   'audio/wav; codecs="2"', 'video/webm;codecs="vp8;opus"', 'video/webm; codecs="vp8"x', 'video/webm; codecs=" vp8 "',
   'video/webm; foo="a;b"; codecs="vp8"', 'video/webm; codecs="vp8', 'video/webm codecs="vp8"', 'video/x-m4v; codecs="avc1.42E01E, mp4a.40.2"',
   'audio/x-m4a; codecs="mp4a.40.2"', 'application/x-mpegurl; codecs="avc1.42E01E, mp4a.40.2"', 'application/x-mpegurl; codecs="mp4a.67"',
   'video/webm; codecs="vp8"; foo', 'vidéo/webm', 'video/webm; codecs="vp8é"', 'video/mp4; codecs="AVC1.42E01E"',
   'video/mp4; codecs="avc1.42e01e"', 'video/mp4; codecs="mp4a.40.+2"', 'video/webm; codecs="vp09.+0.10.08"',
   'video/webm; codecs="vp09.-0.10.08"', 'video/mp4; codecs="av01.0.04M.08 "', 'audio/ogg; codecs=flac', 'audio/ogg; codecs="theora"'
  ].forEach(function (t) { out.push([t, cp(t), ms(t)]); });
  return out;
})()
