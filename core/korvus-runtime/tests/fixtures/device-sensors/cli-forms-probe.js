// which registration form hears the null device events: bare addEventListener, window.addEventListener,
// self.addEventListener, EventTarget.prototype.addEventListener.call(window), window.ondevicemotion
new Promise(function (res) {
  var out = [];
  var forms = {
    bare: function (t, f) { addEventListener(t, f); },
    window: function (t, f) { window.addEventListener(t, f); },
    self: function (t, f) { self.addEventListener(t, f); },
    protoCall: function (t, f) { EventTarget.prototype.addEventListener.call(window, t, f); },
    onprop: function (t, f) { window['on' + t] = f; }
  };
  var names = Object.keys(forms), i = 0;
  function next() {
    if (i >= names.length) { setTimeout(function () { res(out); }, 300); return; }
    var n = names[i++], t0 = performance.now(), got = false;
    forms[n]('devicemotion', function (e) { if (!got) { got = true; out.push([n, Math.round(performance.now() - t0), e.isTrusted]); } });
    setTimeout(function () { if (!got) out.push([n, 'none']); next(); }, 600);
  }
  next();
})
