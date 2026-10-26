(function () {
  // A broken <img> renders Blink's fallback content: the broken-image icon (16x16) and its alt text.
  // Each case is an <img> in its own 300px wrapper; its src is a data: URL that does not decode, so no
  // server is needed. Rows: [label, event, at-event [width, height, complete], settled measurements].
  var BAD = 'data:image/png;base64,AAAA';
  var GOOD = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAADCAIAAAA2iEnWAAAAEElEQVR4nGP4z8AARAwoFABE0AX7pM/egAAAAABJRU5ErkJggg==';
  var cases = [
    ['noalt', {}, ''],
    ['alt-x', {alt: 'x'}, ''],
    ['alt-long', {alt: 'broken image here'}, ''],
    ['title-only', {title: 't'}, ''],
    ['alt-space', {alt: '   '}, ''],
    ['attr30x40', {width: '30', height: '40'}, ''],
    ['attr30x40-alt', {width: '30', height: '40', alt: 'x'}, ''],
    ['attr10x10', {width: '10', height: '10'}, ''],
    ['attr10x10-alt', {width: '10', height: '10', alt: 'x'}, ''],
    ['attr30w', {width: '30'}, ''],
    ['attr30h', {height: '30'}, ''],
    ['css50', {style: 'width:50px;height:50px'}, ''],
    ['css50-alt', {style: 'width:50px;height:50px', alt: 'x'}, ''],
    ['css-pct', {style: 'width:50%'}, ''],
    ['css-ratio', {style: 'width:40px;aspect-ratio:2'}, ''],
    ['block', {style: 'display:block'}, ''],
    ['block-alt', {style: 'display:block', alt: 'x'}, ''],
    ['block-attr100-alt', {style: 'display:block', width: '100', height: '100', alt: 'x'}, ''],
    ['block-attr30w', {style: 'display:block', width: '30'}, ''],
    ['inline-block', {style: 'display:inline-block'}, ''],
    ['inline-block-alt', {style: 'display:inline-block', alt: 'x'}, ''],
    ['alt-empty', {alt: ''}, ''],
    ['nosrc-alt', {alt: 'x', __nosrc: 1}, ''],
    ['nosrc', {__nosrc: 1}, ''],
    ['nosrc-block', {style: 'display:block', __nosrc: 1}, ''],
    ['emptysrc-alt', {alt: 'x', __src: ''}, ''],
    ['emptysrc', {__src: ''}, ''],
    ['flex-item', {}, 'display:flex'],
    ['flex-item-alt', {alt: 'x'}, 'display:flex'],
    ['text-around', {__pre: 'ab', __post: 'cd'}, ''],
    ['text-around-alt', {alt: 'x', __pre: 'ab', __post: 'cd'}, ''],
    ['alt-font32', {alt: 'x', style: 'font-size:32px'}, ''],
    ['border2', {style: 'border:2px solid'}, ''],
    ['padding3', {style: 'padding:3px'}, ''],
    ['padding3-alt', {style: 'padding:3px', alt: 'x'}, ''],
    ['rtl-alt', {alt: 'x', dir: 'rtl'}, ''],
    ['recovered', {__recover: 1}, '']
  ];
  function r2(v) { return Math.round(v * 100) / 100; }
  function m(img, wrap) {
    var r = img.getBoundingClientRect(), cs = getComputedStyle(img);
    var w = wrap ? wrap.getBoundingClientRect() : null;
    return [img.width, img.height, img.naturalWidth, img.naturalHeight, img.complete, img.offsetWidth, img.offsetHeight,
            r2(r.width), r2(r.height), w ? r2(r.left - w.left) : null, w ? r2(r.top - w.top) : null, w ? r2(w.height) : null,
            cs.width, cs.height, cs.display];
  }
  var root = document.createElement('div');
  root.style.cssText = 'position:absolute;left:0;top:0;width:300px';
  document.body.appendChild(root);
  function settle(img, a) {
    return new Promise(function (res) {
      var done = false, at = null;
      img.onerror = function () {
        if (done) return;
        at = [img.width, img.height, img.complete];
        if (a.__recover && img.src !== GOOD) { img.src = GOOD; return; }
        done = true; res(['error', at]);
      };
      img.onload = function () { if (!done) { done = true; res(['load', at || [img.width, img.height, img.complete]]); } };
      setTimeout(function () { if (!done) { done = true; res(['none', at]); } }, 1500);
    });
  }
  var made = cases.map(function (c) {
    var wrap = document.createElement('div');
    wrap.style.cssText = 'width:300px;' + c[2];
    var img = document.createElement('img');
    var a = c[1];
    Object.keys(a).forEach(function (k) { if (k.slice(0, 2) !== '__') img.setAttribute(k, a[k]); });
    if (a.__pre) wrap.appendChild(document.createTextNode(a.__pre));
    wrap.appendChild(img);
    if (a.__post) wrap.appendChild(document.createTextNode(a.__post));
    root.appendChild(wrap);
    var p = settle(img, a);
    if (!a.__nosrc) img.src = ('__src' in a) ? a.__src : BAD;
    return [c[0], img, wrap, p];
  });
  // detached: no box, so width/height read the attribute (HTML's non-negative integer) or the natural size
  var detached = [['detached', {}], ['detached-attr30', {width: '30', height: '40'}], ['detached-attr-px', {width: '30px', height: ' +7.5'}],
                  ['detached-alt', {alt: 'x'}]].map(function (c) {
    var img = document.createElement('img');
    Object.keys(c[1]).forEach(function (k) { img.setAttribute(k, c[1][k]); });
    var p = settle(img, c[1]);
    img.src = BAD;
    return [c[0], img, null, p];
  });
  var all = made.concat(detached);
  return Promise.all(all.map(function (x) { return x[3]; })).then(function (evs) {
    return new Promise(function (res) { setTimeout(function () { res(evs); }, 200); });
  }).then(function (evs) {
    return all.map(function (x, i) { return [x[0], evs[i][0], evs[i][1], m(x[1], x[2])]; });
  });
})()
