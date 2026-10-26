(async () => { const R = [];
  const add = (h) => { const t = document.createElement('template'); t.innerHTML = h; const n = t.content.firstChild; document.head.appendChild(n); return n; };
  add('<style id=a>.a{color:red}</style>'); add('<link id=b rel=stylesheet href="data:text/css,.b{color:blue}">'); add('<link id=c rel="alternate stylesheet" title=t href="data:text/css,.c{}">'); add('<link id=d rel=icon href="data:,">'); add('<link id=e href="data:text/css,.e{}">');
  const det = document.createElement('style'); det.textContent = '.z{}';
  await new Promise((r) => setTimeout(r, 300));
  const desc = (l) => [...l].map((s) => (s.ownerNode && s.ownerNode.id) + ':' + (() => { try { return s.cssRules.length; } catch (e) { return e.name; } })() + ':' + s.type);
  R.push(document.styleSheets.length, desc(document.styleSheets), document.styleSheets === document.styleSheets);
  const host = document.createElement('div'); document.body.appendChild(host); const sr = host.attachShadow({ mode: 'open' }); sr.innerHTML = '<style id=s1>.s{}</style><div><style id=s2>.t{}</style></div>'; R.push(sr.styleSheets.length, desc(sr.styleSheets));
  document.getElementById('a').remove(); R.push(document.styleSheets.length, desc(document.styleSheets));
  const cs = new CSSStyleSheet(); document.adoptedStyleSheets = [cs]; R.push(document.styleSheets.length);
  return R; })()
