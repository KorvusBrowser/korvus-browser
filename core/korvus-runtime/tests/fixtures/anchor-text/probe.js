(() => { const R = []; const a = document.createElement('a'); a.innerHTML = 'x<b>y</b>z'; document.body.appendChild(a); R.push(a.text); a.text = null; R.push(a.text, a.childNodes.length); a.text = { toString() { return 'obj'; } }; R.push(a.text); a.text = ''; R.push(a.childNodes.length, a.text);
  const mo = new MutationObserver(() => {}); mo.observe(a, { childList: true }); a.text = 'q'; R.push(mo.takeRecords().map((r) => r.type + r.addedNodes.length + r.removedNodes.length).join());
  R.push(window.find('qq'), window.find('')); return R; })()
