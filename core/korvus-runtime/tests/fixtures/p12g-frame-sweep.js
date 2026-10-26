(async function () {
  var W = window, P = parent;
  var noop = function () {};
  addEventListener("error", function (e) { e.preventDefault(); });
  var fns = {
    fetch: function (f, r) { return f.call(r, P.location.href); },
    setTimeout: function (f, r) { return f.call(r, noop, 0); },
    setInterval: function (f, r) { var id = f.call(r, noop, 100000); clearInterval(id); return id; },
    clearTimeout: function (f, r) { return f.call(r, 0); },
    clearInterval: function (f, r) { return f.call(r, 0); },
    queueMicrotask: function (f, r) { return f.call(r, noop); },
    structuredClone: function (f, r) { return f.call(r, 1); },
    atob: function (f, r) { return f.call(r, "YQ=="); },
    btoa: function (f, r) { return f.call(r, "a"); },
    requestAnimationFrame: function (f, r) { return f.call(r, noop); },
    cancelAnimationFrame: function (f, r) { return f.call(r, 0); },
    requestIdleCallback: function (f, r) { return f.call(r, noop); },
    cancelIdleCallback: function (f, r) { return f.call(r, 0); },
    getComputedStyle: function (f, r) { return f.call(r, document.documentElement); },
    matchMedia: function (f, r) { return f.call(r, "all"); },
    postMessage: function (f, r) { return f.call(r, "x", "*"); },
    createImageBitmap: function (f, r) { return f.call(r, new ImageData(1, 1)); },
    reportError: function (f, r) { return f.call(r, new Error("x")); },
    alert: function (f, r) { return f.call(r); },
    confirm: function (f, r) { return f.call(r); },
    prompt: function (f, r) { return f.call(r); },
    print: function (f, r) { return f.call(r); },
    open: function (f, r) { return f.call(r, "about:blank", "p12g"); },
    scroll: function (f, r) { return f.call(r, 0, 0); },
    scrollTo: function (f, r) { return f.call(r, 0, 0); },
    scrollBy: function (f, r) { return f.call(r, 0, 0); },
    getSelection: function (f, r) { return f.call(r); },
    focus: function (f, r) { return f.call(r); },
    blur: function (f, r) { return f.call(r); },
    stop: function (f, r) { return f.call(r); }
  };
  var direct = {
    fetch: function () { return fetch(P.location.href); },
    setTimeout: function () { return setTimeout(noop, 0); },
    setInterval: function () { var id = setInterval(noop, 100000); clearInterval(id); return id; },
    clearTimeout: function () { return clearTimeout(0); },
    clearInterval: function () { return clearInterval(0); },
    queueMicrotask: function () { return queueMicrotask(noop); },
    structuredClone: function () { return structuredClone(1); },
    atob: function () { return atob("YQ=="); },
    btoa: function () { return btoa("a"); },
    requestAnimationFrame: function () { return requestAnimationFrame(noop); },
    cancelAnimationFrame: function () { return cancelAnimationFrame(0); },
    requestIdleCallback: function () { return requestIdleCallback(noop); },
    cancelIdleCallback: function () { return cancelIdleCallback(0); },
    getComputedStyle: function () { return getComputedStyle(document.documentElement); },
    matchMedia: function () { return matchMedia("all"); },
    postMessage: function () { return postMessage("x", "*"); },
    createImageBitmap: function () { return createImageBitmap(new ImageData(1, 1)); },
    reportError: function () { return reportError(new Error("x")); },
    alert: function () { return alert(); },
    confirm: function () { return confirm(); },
    prompt: function () { return prompt(); },
    print: function () { return print(); },
    open: function () { return open("about:blank", "p12g"); },
    scroll: function () { return scroll(0, 0); },
    scrollTo: function () { return scrollTo(0, 0); },
    scrollBy: function () { return scrollBy(0, 0); },
    getSelection: function () { return getSelection(); },
    focus: function () { return focus(); },
    blur: function () { return blur(); },
    stop: function () { return stop(); }
  };
  var recv = {
    bare: function () { return undefined; },
    self: function () { return W; },
    foreign: function () { return {}; },
    parent: function () { return P; }
  };
  function tag(v) { return v === null ? "null" : Object.prototype.toString.call(v).slice(8, -1); }
  function settle(p) {
    return Promise.race([
      p.then(function (v) { return "res:" + tag(v); }, function (e) { return "rej:" + (e && e.message); }),
      new Promise(function (ok) { setTimeout(function () { ok("pending"); }, 1500); })
    ]);
  }
  var out = {};
  var only = (P.location.search.match(/only=([^&]*)/) || [])[1];
  for (var name in fns) {
    if (only && only.split(",").indexOf(name) < 0) continue;
    var row = {};
    for (var rk in recv) {
      var cell;
      try {
        var f = W[name];
        if (typeof f !== "function") { cell = "absent"; }
        else {
          var v = fns[name](f, recv[rk]());
          cell = (v && typeof v.then === "function") ? await settle(v) : "ok:" + tag(v);
        }
      } catch (e) { cell = "throw:" + (e && e.message); }
      row[rk] = cell;
    }
    try {
      var v3 = direct[name]();
      row.direct = (v3 && typeof v3.then === "function") ? await settle(v3) : "ok:" + tag(v3);
    } catch (e) { row.direct = "throw:" + (e && e.message); }
    try {
      var pf = P[name];
      var v2 = fns[name](pf, W);
      row.pageFnFrameRecv = (v2 && typeof v2.then === "function") ? await settle(v2) : "ok:" + tag(v2);
    } catch (e) { row.pageFnFrameRecv = "throw:" + (e && e.message); }
    out[name] = row;
  }
  P.RESULT = JSON.stringify(out);
})();
