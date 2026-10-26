(async function () {
  /* toDataURL / toBlob through every encoder Blink has, with the quality argument in each shape: the type each answers,
     its length, its SHA-256 (the bytes) and, when short, the whole URL. */
  var out = [];
  var hex = async function (s) {
    var b = new TextEncoder().encode(s);
    var h = new Uint8Array(await crypto.subtle.digest('SHA-256', b));
    return Array.prototype.map.call(h, function (x) { return ('0' + x.toString(16)).slice(-2); }).join('').slice(0, 24);
  };
  var bhex = async function (blob) {
    var h = new Uint8Array(await crypto.subtle.digest('SHA-256', await blob.arrayBuffer()));
    return Array.prototype.map.call(h, function (x) { return ('0' + x.toString(16)).slice(-2); }).join('').slice(0, 24);
  };
  var mk = function (w, h, draw) { var c = document.createElement('canvas'); c.width = w; c.height = h; var g = c.getContext('2d'); draw(g, w, h); return c; };
  var canvases = {
    red16: mk(16, 16, function (g) { g.fillStyle = '#f00'; g.fillRect(0, 0, 16, 16); }),
    two37: mk(37, 23, function (g) { g.fillStyle = 'rgb(10,200,30)'; g.fillRect(0, 0, 37, 23); g.fillStyle = 'rgb(240,20,90)'; g.fillRect(5, 4, 20, 11); }),
    alpha20: mk(20, 10, function (g) { g.fillStyle = 'rgba(0,128,255,0.5)'; g.fillRect(2, 2, 12, 6); }),
    grad64: mk(64, 32, function (g) { var gr = g.createLinearGradient(0, 0, 64, 0); gr.addColorStop(0, '#123456'); gr.addColorStop(1, '#fedcba'); g.fillStyle = gr; g.fillRect(0, 0, 64, 32); }),
    blank8: mk(8, 8, function () {}),
    zero: mk(0, 0, function () {})
  };
  var args = [
    ['image/webp'], ['image/webp', 0.5], ['image/webp', 1], ['image/webp', 1.0000001], ['image/webp', '0.5'], ['image/webp', 2],
    ['image/webp', -0.1], ['image/webp', 0], ['image/webp', NaN], ['IMAGE/WEBP'], [' image/webp'],
    ['image/jpeg'], ['image/jpeg', 0.5], ['image/jpeg', '0.5'], ['image/jpeg', 1], ['image/jpeg', true],
    ['image/png'], ['image/png', 0.1], ['image/bmp'], [], ['image/avif']
  ];
  for (var name in canvases) {
    var c = canvases[name];
    for (var i = 0; i < args.length; i++) {
      var a = args[i];
      var url;
      try { url = c.toDataURL.apply(c, a); } catch (e) { out.push([name, JSON.stringify(a), 'THROW ' + e.name]); continue; }
      var head = url.slice(0, url.indexOf(',') + 1);
      out.push([name, JSON.stringify(a), head, url.length, await hex(url), url.length < 420 ? url : '']);
    }
  }
  var blobs = [['image/webp'], ['image/webp', 0.5], ['image/webp', '0.5'], ['image/webp', 1], ['image/jpeg', 0.3], ['image/jpeg', '0.3'], ['image/png'], ['image/x-nope']];
  for (var name2 of ['two37', 'alpha20', 'zero']) {
    for (var j = 0; j < blobs.length; j++) {
      var b = blobs[j];
      var blob = await new Promise(function (res) { var cc = canvases[name2]; cc.toBlob.apply(cc, [function (x) { res(x); }].concat(b)); });
      out.push(['toBlob ' + name2, JSON.stringify(b), blob ? blob.type : null, blob ? blob.size : null, blob ? await bhex(blob) : null]);
    }
  }
  var oc = new OffscreenCanvas(37, 23); var og = oc.getContext('2d'); og.fillStyle = 'rgb(10,200,30)'; og.fillRect(0, 0, 37, 23); og.fillStyle = 'rgb(240,20,90)'; og.fillRect(5, 4, 20, 11);
  for (var o of [{ type: 'image/webp' }, { type: 'image/webp', quality: 0.5 }, { type: 'image/webp', quality: 1 }, { type: 'image/jpeg', quality: 0.3 }]) {
    try { var ob = await oc.convertToBlob(o); out.push(['convertToBlob', JSON.stringify(o), ob.type, ob.size, await bhex(ob)]); } catch (e) { out.push(['convertToBlob', JSON.stringify(o), 'THROW ' + e.name]); }
  }
  return out;
})()
