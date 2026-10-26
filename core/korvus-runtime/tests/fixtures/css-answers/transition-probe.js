(function () {
  /* A CSS transition starts when the style changes against a before-change style the engine has already computed --
     and Blink computes it at every style-and-layout flush, not only at getComputedStyle (a bot-detection service's VM reads offsetHeight).
     Each row: a fresh element with `transition: opacity 5s linear`, one way of flushing, `opacity = 0.3`, then what
     getComputedStyle says and how many animations the element has. */
  var out = [];
  function run(label, settle) {
    var d = document.createElement('div');
    d.style.cssText = 'position:absolute;left:-9999px;display:block;opacity:1;transition:opacity 5000ms linear;';
    document.body.appendChild(d);
    settle(d);
    d.style.opacity = '0.3';
    var a = d.getAnimations();
    out.push([label, getComputedStyle(d).opacity, a.length, a.length ? a[0].constructor.name + ':' + a[0].transitionProperty : '']);
    d.remove();
  }
  run('offsetHeight', function (d) { void d.offsetHeight; });
  run('offsetWidth', function (d) { void d.offsetWidth; });
  run('offsetTop', function (d) { void d.offsetTop; });
  run('clientWidth', function (d) { void d.clientWidth; });
  run('scrollHeight', function (d) { void d.scrollHeight; });
  run('getBoundingClientRect', function (d) { void d.getBoundingClientRect().width; });
  run('getClientRects', function (d) { void d.getClientRects().length; });
  run('getComputedStyle(d).opacity', function (d) { void getComputedStyle(d).opacity; });
  run('getComputedStyle(d) unread', function (d) { getComputedStyle(d); });
  run('getComputedStyle(body).color', function (d) { void getComputedStyle(document.body).color; });
  run('body.offsetHeight', function (d) { void document.body.offsetHeight; });
  run('documentElement.scrollTop', function (d) { void document.documentElement.scrollTop; });
  run('no flush', function (d) {});
  run('innerHTML read', function (d) { void d.innerHTML; });
  // a change in the opposite direction while one runs
  var d = document.createElement('div');
  d.style.cssText = 'position:absolute;left:-9999px;opacity:1;transition:opacity 5000ms linear;';
  document.body.appendChild(d);
  void d.offsetHeight;
  d.style.opacity = '0';
  void d.offsetHeight;
  var a = d.getAnimations();
  out.push(['running after offsetHeight', a.length, a.length ? a[0].playState : '', a.length ? JSON.stringify(a[0].effect.getKeyframes().map(function (k) { return [k.offset, k.opacity]; })) : '']);
  d.style.transition = 'none';
  void d.offsetHeight;
  out.push(['transition none then flush', d.getAnimations().length, getComputedStyle(d).opacity]);
  d.remove();
  return out;
})()
