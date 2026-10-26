(async () => {
  // Every call the ENGINE makes through something a page can replace, while it runs ordinary operations:
  // each interface prototype's methods, getters and setters, the window's own members, and the global
  // names themselves (a constructor or an ES global the engine looks up by name). Recorded only when made
  // from INSIDE another wrapped call -- the engine's work on the page's behalf. Chrome 150: none.
  // the probe's own machinery, taken before the global names are wrapped (a read of one is recorded)
  const { construct: rConstruct, apply: rApply, get: rGet } = Reflect;
  const ProxyC = Proxy, MapC = Map;
  const nested = new Map();
  let depth = 0;
  let cur = '';
  const rec = (label) => { if (depth > 0) { const k = cur + ' -> ' + label; nested.set(k, (nested.get(k) || 0) + 1); } };
  const wrapFn = (o, label) => function () {
    rec(label); const prev = cur; cur = label; depth++;
    try { return new.target ? rConstruct(o, arguments, new.target) : rApply(o, this, arguments); } finally { depth--; cur = prev; }
  };
  const wrapAccessor = (obj, k, d, label) => {
    const g = d.get, s = d.set;
    const nd = { configurable: true, enumerable: d.enumerable };
    if (g) nd.get = function () { rec('get ' + label); const prev = cur; cur = 'get ' + label; depth++; try { return g.call(this); } finally { depth--; cur = prev; } };
    if (s) nd.set = function (v) { rec('set ' + label); const prev = cur; cur = 'set ' + label; depth++; try { return s.call(this, v); } finally { depth--; cur = prev; } };
    try { Object.defineProperty(obj, k, nd); } catch (e) {}
  };
  const ES = /^(Object|Function|Array|Promise|Symbol|Error|Map|Set|WeakMap|WeakSet|Proxy|Reflect|JSON|Math|Date|RegExp|String|Number|Boolean|BigInt|.*Array|ArrayBuffer|SharedArrayBuffer|DataView|Atomics|.*Error|Intl|WebAssembly|Iterator|AsyncIterator|FinalizationRegistry|WeakRef|globalThis|Infinity|NaN|undefined|eval|isFinite|isNaN|parseFloat|parseInt|decodeURI|decodeURIComponent|encodeURI|encodeURIComponent|escape|unescape|AggregateError|SuppressedError|DisposableStack|AsyncDisposableStack|Float16Array)$/;
  // 1. interface prototypes: methods, getters, setters (each prototype once: Image/Option share theirs)
  const protos = new Set();
  const names = Object.getOwnPropertyNames(window).filter((n) => /^[A-Z]/.test(n) && !ES.test(n) && typeof window[n] === 'function' && window[n].prototype);
  for (const n of names) {
    const p = window[n].prototype;
    if (protos.has(p)) continue;
    protos.add(p);
    for (const k of Object.getOwnPropertyNames(p)) {
      if (k === 'constructor') continue;
      const d = Object.getOwnPropertyDescriptor(p, k);
      if (!d || !d.configurable) continue;
      const label = n + '.' + k;
      if (typeof d.value === 'function') { try { Object.defineProperty(p, k, { ...d, value: wrapFn(d.value, label) }); } catch (e) {} }
      else if (d.get || d.set) wrapAccessor(p, k, d, label);
    }
  }
  // 2. the window's own members and the global names: a read of the NAME is recorded (an engine that looks a
  //    constructor or an ES global up by name runs whatever the page put there), a method is wrapped as above
  const own = Object.getOwnPropertyNames(window);
  for (const k of own) {
    const d = Object.getOwnPropertyDescriptor(window, k);
    if (!d || !d.configurable || k === '__done') continue;
    const label = 'window.' + k;
    if (d.get || d.set) { wrapAccessor(window, k, d, label); continue; }
    const v = d.value;
    const isCtor = typeof v === 'function' && /^[A-Z]/.test(k);
    // an interface constructor: its construction and its static methods are wrapped calls too (an ES
    // constructor is served as it is -- the harness's own Promise runs through it)
    let served = v;
    if (typeof v === 'function' && !isCtor) served = wrapFn(v, label);
    else if (isCtor && !ES.test(k) && v.prototype) {
      const statics = new MapC();
      served = new ProxyC(v, {
        construct(t, a, nt) { rec('new ' + k); const prev = cur; cur = 'new ' + k; depth++; try { return rConstruct(t, a, nt === served ? t : nt); } finally { depth--; cur = prev; } },
        apply(t, th, a) { rec(k + '()'); const prev = cur; cur = k + '()'; depth++; try { return rApply(t, th, a); } finally { depth--; cur = prev; } },
        get(t, p, r) {
          const x = rGet(t, p, t);
          if (typeof x !== 'function' || p === 'prototype' || typeof p !== 'string') return x;
          if (!statics.has(p)) statics.set(p, wrapFn(x, k + '.' + p));
          return statics.get(p);
        },
      });
    }
    try {
      Object.defineProperty(window, k, {
        configurable: true, enumerable: d.enumerable,
        get() { rec('get ' + label); return served; },
        set(x) { Object.defineProperty(window, k, { value: x, writable: true, configurable: true, enumerable: d.enumerable }); },
      });
    } catch (e) {}
  }
  // operations
  const T = async (f) => { try { await f(); } catch (e) {} };
  const host = document.createElement('div'); document.body.appendChild(host);
  await T(() => { const c = document.createElement('canvas'); c.width = 40; c.height = 30; host.appendChild(c); const x = c.getContext('2d'); x.fillRect(0, 0, 5, 5); const id = x.getImageData(0, 0, 4, 4); x.putImageData(id, 1, 1); x.createImageData(id); c.toDataURL(); });
  await T(() => { const b = new Blob(['1'], { type: 'text/javascript' }); const u = URL.createObjectURL(b); const w = new Worker(u); w.postMessage(1); w.terminate(); URL.revokeObjectURL(u); });
  await T(() => { const po = new PerformanceObserver(() => {}); po.observe({ type: 'mark' }); performance.mark('m'); po.takeRecords(); po.disconnect(); });
  await T(() => { document.title; document.title = 't1'; document.title = 't2'; });
  await T(() => { const st = document.createElement('style'); st.textContent = '.q{color:red}'; document.head.appendChild(st); st.sheet.cssRules.length; st.textContent = '.q{color:blue}'; st.sheet.cssRules[0].cssText; getComputedStyle(host).color; });
  await T(() => { const t = document.createElement('table'); host.appendChild(t); const r = t.insertRow(); r.insertCell(); r.insertCell(0); t.createTHead(); t.createTFoot(); t.createTBody(); t.createCaption(); t.rows.length; r.cells.length; r.rowIndex; r.sectionRowIndex; t.deleteRow(0); t.tBodies.length; t.deleteCaption(); t.deleteTHead(); t.deleteTFoot(); const tb = t.tBodies[0]; tb.insertRow().insertCell().cellIndex; tb.deleteRow(0); });
  await T(() => { const f = document.createElement('form'); f.id = 'f1'; host.appendChild(f); const i = document.createElement('input'); i.id = 'i1'; i.name = 'nm'; f.appendChild(i); const l = document.createElement('label'); l.htmlFor = 'i1'; l.textContent = 'L'; f.appendChild(l); l.control; l.form; i.labels.length; i.form; f.elements.nm; f.elements.length; document.forms.length; new FormData(f); i.value = 'v'; i.checkValidity(); f.reset(); l.click(); const l2 = document.createElement('label'); const cb = document.createElement('input'); cb.type = 'checkbox'; l2.appendChild(cb); f.appendChild(l2); l2.click(); cb.click(); const rb = document.createElement('input'); rb.type = 'radio'; rb.name = 'r'; f.appendChild(rb); rb.click(); const sb = document.createElement('button'); f.appendChild(sb); f.onsubmit = (e) => e.preventDefault(); sb.click(); f.requestSubmit(); });
  await T(() => { const s = document.createElement('select'); host.appendChild(s); const o = new Option('t1', 'v1'); s.add(o); s.add(new Option('t2', 'v2'), 0); o.text; o.index; o.label; o.form; s.value; s.selectedIndex = 1; s.options.length; s.selectedOptions.length; o.text = 'n'; s.remove(0); s.options.add(new Option('t3')); s.length = 1; });
  await T(() => { const d = document.createElement('div'); d.innerHTML = '<b>a</b>c<i>d</i>'; host.appendChild(d); const r = document.createRange(); r.selectNodeContents(d); r.toString(); r.cloneContents(); r.getBoundingClientRect(); r.getClientRects(); r.setStart(d.firstChild, 0); r.setEnd(d.lastChild, 0); r.extractContents(); r.collapse(); r.insertNode(document.createElement('u')); r.surroundContents(document.createElement('s')); r.deleteContents(); r.commonAncestorContainer; const sel = getSelection(); sel.removeAllRanges(); sel.addRange(r); sel.toString(); sel.collapse(d, 0); sel.extend(d, 1); sel.getRangeAt(0); sel.deleteFromDocument(); });
  await T(() => { const a = document.createElement('a'); a.href = '#x'; a.textContent = 'link'; host.appendChild(a); a.text; a.text = 't2'; a.click(); a.hash; a.origin; });
  await T(() => { new XMLSerializer().serializeToString(host); });
  await T(() => { document.body.onload = () => {}; host.setAttribute('onclick', 'void 0'); host.onclick; document.body.setAttribute('onresize', 'void 0'); });
  await T(() => { host.animate([{ opacity: 0 }, { opacity: 1 }], 100); document.timeline.currentTime; document.getAnimations(); });
  await T(() => { window.find('link'); });
  await T(() => { host.innerText; host.innerText = 'x'; host.outerHTML; const c = document.createElement('p'); host.appendChild(c); c.outerHTML = '<span>o</span>'; host.insertAdjacentElement('beforeend', document.createElement('em')); host.insertAdjacentText('beforeend', 't'); host.replaceChildren(document.createElement('div')); host.normalize(); document.importNode(host, true); document.adoptNode(document.createElement('q')); new DOMParser().parseFromString('<p>x', 'text/html').body.innerHTML; host.matches('div'); host.closest('body'); document.querySelectorAll('div'); document.getElementsByClassName('q'); });
  await T(() => { const dt = document.createElement('details'); dt.innerHTML = '<summary>s</summary>x'; host.appendChild(dt); dt.open = true; const dg = document.createElement('dialog'); host.appendChild(dg); dg.showModal(); dg.close(); });
  await T(() => { const mo = new MutationObserver(() => {}); mo.observe(host, { childList: true, subtree: true, attributes: true, characterData: true }); host.setAttribute('data-x', '1'); host.appendChild(document.createTextNode('t')); mo.takeRecords(); new IntersectionObserver(() => {}).observe(host); new ResizeObserver(() => {}).observe(host); });
  await T(() => { const ta = document.createElement('textarea'); ta.textContent = 'z'; host.appendChild(ta); ta.value; ta.defaultValue; ta.select(); ta.setRangeText('q'); ta.setSelectionRange(0, 1); const ib = document.createElement('input'); host.appendChild(ib); ib.value = 'abc'; ib.select(); ib.setRangeText('q', 0, 1); ib.focus(); ib.blur(); document.activeElement; });
  await T(() => { const im = document.createElement('img'); im.src = 'data:image/gif;base64,R0lGODlhAQABAAAAACw='; host.appendChild(im); im.width; im.naturalWidth; im.currentSrc; im.decode().catch(() => {}); const ifr = document.createElement('iframe'); ifr.srcdoc = '<p>x'; host.appendChild(ifr); ifr.contentWindow; ifr.contentDocument; });
  await T(() => { document.cookie; document.referrer; document.dir = 'ltr'; document.documentElement.lang = 'en'; document.characterSet; document.hasFocus(); document.elementFromPoint(1, 1); document.elementsFromPoint(1, 1); document.caretPositionFromPoint && document.caretPositionFromPoint(1, 1); });
  await T(() => { for (let i = 0; i < 3; i++) { const ss = document.styleSheets; ss.length; for (const sh of ss) { sh.cssRules && sh.cssRules.length; sh.href; sh.ownerNode; } } });
  await T(() => { history.replaceState({ a: 1 }, '', '#h'); history.state; location.hash = 'h2'; });
  await T(() => { postMessage('m', '*'); const mc = new MessageChannel(); mc.port1.postMessage('p'); mc.port2.onmessage = () => {}; new BroadcastChannel('b').postMessage('x'); });
  await T(() => { const x = new XMLHttpRequest(); x.open('GET', '/nothing'); x.send(); return fetch('/nothing').then((r) => r.text()).catch(() => {}); });
  await T(() => { customElements.define('x-q', class extends HTMLElement { connectedCallback() {} }); host.appendChild(document.createElement('x-q')); });
  await T(() => { structuredClone({ a: [1, new Date(0)] }); queueMicrotask(() => {}); requestAnimationFrame(() => {}); atob(btoa('x')); });
  await new Promise((r) => setTimeout(r, 400));
  return [...nested.entries()].map(([k, v]) => k + ' x' + v).sort();
})()
