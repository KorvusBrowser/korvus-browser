(function () {
  // chrome.app is an extension binding: the extensions dispatcher defers a main-world context made while its frame
  // is requesting a navigation until that navigation is ready to commit (ExtensionFrameHelper::DidCreateScriptContext).
  var out = [];
  function rec(label, w) {
    try { out.push([label, typeof w.chrome + '/' + (w.chrome ? typeof w.chrome.app : '-') + '/' + (w.chrome ? Object.getOwnPropertyNames(w.chrome).join(',') : '-') + ' href=' + w.location.href]); }
    catch (e) { out.push([label, 'THROW ' + e]); }
  }
  // S1: a sensor cookie shape -- <iframe srcdoc> parsed into a hidden div, its window read at once
  var d = document.createElement('div'); d.style.display = 'none'; d.innerHTML = '<iframe srcdoc=1></iframe>';
  document.body.appendChild(d);
  var f1 = d.querySelector('iframe'), w1 = f1.contentWindow, c1 = w1.chrome;
  rec('S1 srcdoc in markup, at insertion', w1);
  // S2: a created iframe given a srcdoc before insertion
  var f2 = document.createElement('iframe'); f2.srcdoc = '<p>x</p>'; document.body.appendChild(f2);
  rec('S2 srcdoc set before append, at insertion', f2.contentWindow);
  // S3: a same-origin src
  var f3 = document.createElement('iframe'); f3.src = '/framed'; document.body.appendChild(f3);
  rec('S3 same-origin src, at insertion', f3.contentWindow);
  // S4: no src, the window read, then srcdoc set
  var f4 = document.createElement('iframe'); document.body.appendChild(f4);
  rec('S4 no src, at insertion', f4.contentWindow);
  f4.srcdoc = '<p>y</p>';
  rec('S4 then srcdoc set', f4.contentWindow);
  // S5: no src, NOT read, then srcdoc set, then read
  var f5 = document.createElement('iframe'); document.body.appendChild(f5);
  f5.srcdoc = '<p>z</p>';
  rec('S5 untouched, srcdoc set, then read', f5.contentWindow);
  // S6: an explicit about:blank
  var f6 = document.createElement('iframe'); f6.src = 'about:blank'; document.body.appendChild(f6);
  rec('S6 src about:blank, at insertion', f6.contentWindow);
  // S7: a frame with no src, read a task later
  var f7 = document.createElement('iframe'); document.body.appendChild(f7);
  rec('S0 top window', window);
  return new Promise(function (res) {
    setTimeout(function () {
      rec('S1 +600ms', f1.contentWindow);
      out.push(['S1 the chrome object held at insertion, +600ms', typeof c1.app + ' same:' + (f1.contentWindow.chrome === c1) + ' keys:' + Object.keys(c1).join(',')]);
      rec('S1 the window read at insertion, +600ms', w1);
      rec('S2 +600ms', f2.contentWindow);
      rec('S3 +600ms', f3.contentWindow);
      rec('S4 +600ms', f4.contentWindow);
      rec('S5 +600ms', f5.contentWindow);
      rec('S6 +600ms', f6.contentWindow);
      rec('S7 +600ms', f7.contentWindow);
      res(out);
    }, 600);
  });
})()
