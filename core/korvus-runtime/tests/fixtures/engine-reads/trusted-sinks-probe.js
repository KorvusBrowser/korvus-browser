(async () => {
  // A Trusted Types sink reads a trusted value of its own union type off the value itself -- Blink's binding takes
  // the TrustedHTML / TrustedScript / TrustedScriptURL as is -- and calls nothing on it; a value of another type is
  // a plain object to it, stringified (toString runs). Every row: what the page's wrappers saw, then the effect.
  const R = [];
  const log = [];
  for (const I of [TrustedHTML, TrustedScript, TrustedScriptURL]) {
    const p = I.prototype;
    const orig = p.toString;
    p.toString = function () { log.push(I.name + '.toString'); return orig.call(this); };
    Object.defineProperty(p, Symbol.toPrimitive, { configurable: true, get() { log.push(I.name + '.@@toPrimitive'); return undefined; } });
  }
  const take = () => { const l = log.join(','); log.length = 0; return l; };
  const pol = trustedTypes.createPolicy('probe', { createHTML: (s) => s, createScript: (s) => s, createScriptURL: (s) => s });
  const H = (s) => pol.createHTML(s);
  const S = (s) => pol.createScript(s);
  const U = (s) => pol.createScriptURL(s);
  take();
  const row = (name, f) => {
    let effect;
    try { effect = f(); } catch (e) { effect = 'threw ' + e.name; }
    R.push([name, take(), typeof effect === 'string' ? effect : JSON.stringify(effect)]);
  };
  const div = () => document.createElement('div');

  row('innerHTML = TrustedHTML', () => { const d = div(); d.innerHTML = H('<b>a</b>'); return d.innerHTML; });
  row('innerHTML = TrustedScript (not its type)', () => { const d = div(); d.innerHTML = S('<i>s</i>'); return d.innerHTML; });
  row('outerHTML = TrustedHTML', () => { const d = div(); const c = d.appendChild(div()); c.outerHTML = H('<p>o</p>'); return d.innerHTML; });
  row('insertAdjacentHTML(TrustedHTML)', () => { const d = div(); d.insertAdjacentHTML('beforeend', H('<u>x</u>')); return d.innerHTML; });
  row('setHTMLUnsafe(TrustedHTML)', () => { const d = div(); if (!d.setHTMLUnsafe) return 'absent'; d.setHTMLUnsafe(H('<s>u</s>')); return d.innerHTML; });
  row('shadowRoot.innerHTML = TrustedHTML', () => { const d = div(); const sr = d.attachShadow({ mode: 'open' }); sr.innerHTML = H('<em>r</em>'); return sr.innerHTML; });
  row('iframe.srcdoc = TrustedHTML', () => { const f = document.createElement('iframe'); f.srcdoc = H('<p>d</p>'); return f.getAttribute('srcdoc'); });
  row('Range.createContextualFragment(TrustedHTML)', () => { const d = div(); document.body.append(d); const r = document.createRange(); r.selectNodeContents(d); const fr = r.createContextualFragment(H('<a>f</a>')); d.remove(); return fr.childNodes.length + ':' + fr.firstChild.nodeName; });
  row('DOMParser.parseFromString(TrustedHTML)', () => new DOMParser().parseFromString(H('<p>z</p>'), 'text/html').body.innerHTML);
  row('Document.parseHTMLUnsafe(TrustedHTML)', () => Document.parseHTMLUnsafe ? Document.parseHTMLUnsafe(H('<p>q</p>')).body.innerHTML : 'absent');
  row('setAttribute(name, TrustedHTML)', () => { const d = div(); d.setAttribute('data-h', H('h1')); return d.getAttribute('data-h'); });
  row('setAttribute(name, TrustedScript)', () => { const d = div(); d.setAttribute('data-s', S('s1')); return d.getAttribute('data-s'); });
  row('setAttribute(name, TrustedScriptURL)', () => { const d = div(); d.setAttribute('data-u', U('u1')); return d.getAttribute('data-u'); });
  row('setAttributeNS(null, name, TrustedScriptURL)', () => { const d = div(); d.setAttributeNS(null, 'data-n', U('n1')); return d.getAttribute('data-n'); });
  row('script.src = TrustedScriptURL', () => { const s = document.createElement('script'); s.src = U('/nothing.js'); return s.getAttribute('src'); });
  row('script.src = TrustedHTML (not its type)', () => { const s = document.createElement('script'); s.src = H('/h.js'); return s.getAttribute('src'); });
  row('script.text = TrustedScript', () => { const s = document.createElement('script'); s.text = S('var t1 = 1;'); return s.text; });
  row('script.textContent = TrustedScript', () => { const s = document.createElement('script'); s.textContent = S('var t2 = 2;'); return s.textContent; });
  row('script.innerText = TrustedScript', () => { const s = document.createElement('script'); s.innerText = S('var t3 = 3;'); return s.textContent; });
  row('div.textContent = TrustedScript', () => { const d = div(); d.textContent = S('tc'); return d.textContent; });
  row('div.innerText = TrustedScript', () => { const d = div(); d.innerText = S('it'); return d.textContent; });
  row('append(TrustedScript)', () => { const d = div(); d.append(S('ap')); return d.textContent; });
  row('prepend(TrustedScript)', () => { const d = div(); d.prepend(S('pp')); return d.textContent; });
  row('replaceChildren(TrustedScript)', () => { const d = div(); d.replaceChildren(S('rc')); return d.textContent; });
  row('before/after/replaceWith(TrustedScript)', () => { const d = div(); const c = d.appendChild(div()); c.before(S('b')); c.after(S('a')); c.replaceWith(S('w')); return d.textContent; });
  row('append(TrustedHTML) (not its type)', () => { const d = div(); d.append(H('<b>x</b>')); return d.textContent; });
  row('embed.src = TrustedScriptURL', () => { const e = document.createElement('embed'); e.src = U('/e.bin'); return e.getAttribute('src'); });
  row('object.data = TrustedScriptURL', () => { const o = document.createElement('object'); o.data = U('/o.bin'); return o.getAttribute('data'); });
  row('object.codeBase = TrustedScriptURL', () => { const o = document.createElement('object'); o.codeBase = U('/cb/'); return o.getAttribute('codebase'); });
  row('svg script href.baseVal = TrustedScriptURL', () => { const s = document.createElementNS('http://www.w3.org/2000/svg', 'script'); s.href.baseVal = U('/svg.js'); return s.getAttribute('href'); });
  row('new Worker(TrustedScriptURL)', () => { const u = URL.createObjectURL(new Blob(['postMessage(1)'], { type: 'text/javascript' })); const w = new Worker(U(u)); w.terminate(); return typeof w; });
  row('new Worker(TrustedHTML) (not its type)', () => { const u = URL.createObjectURL(new Blob([''], { type: 'text/javascript' })); const w = new Worker(H(u)); w.terminate(); return typeof w; });
  row('setTimeout(TrustedScript)', () => { const id = setTimeout(S('window.__tt1 = 1'), 0); clearTimeout(id); return typeof id; });
  return R;
})()
