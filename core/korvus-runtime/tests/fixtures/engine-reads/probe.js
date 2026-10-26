(async () => {
  const nested = new Map(); let depth = 0; let cur = '';
  const skip = new Set(['constructor']);
  const wrapped = new WeakSet();
  const names = Object.getOwnPropertyNames(window).filter((n) => /^[A-Z]/.test(n) && typeof window[n] === 'function' && window[n].prototype && !/^(Object|Function|Array|Promise|Symbol|Error|Map|Set|WeakMap|WeakSet|Proxy|Reflect|JSON|Math|Date|RegExp|String|Number|Boolean|BigInt|.*Array|ArrayBuffer|SharedArrayBuffer|DataView|Atomics|.*Error|Intl|WebAssembly|Iterator|AsyncIterator|FinalizationRegistry|WeakRef)$/.test(n));
  const rec = (label) => { if (depth > 0) { const k = cur + ' -> ' + label; nested.set(k, (nested.get(k) || 0) + 1); } };
  for (const n of names) {
    const p = window[n].prototype;
    for (const k of Object.getOwnPropertyNames(p)) {
      if (skip.has(k)) continue;
      const d = Object.getOwnPropertyDescriptor(p, k);
      if (!d || !d.configurable) continue;
      const label = n + '.' + k;
      if (typeof d.value === 'function') {
        const o = d.value;
        if (wrapped.has(o)) continue;
        const w = function () { rec(label); const prev = cur; cur = label; depth++; try { return o.apply(this, arguments); } finally { depth--; cur = prev; } };
        wrapped.add(w);
        try { Object.defineProperty(p, k, { ...d, value: w }); } catch (e) {}
      } else if (d.get) {
        const g = d.get;
        const w = function () { rec('get ' + label); const prev = cur; cur = 'get ' + label; depth++; try { return g.apply(this, arguments); } finally { depth--; cur = prev; } };
        try { Object.defineProperty(p, k, { ...d, get: w }); } catch (e) {}
      }
    }
  }
  // operations
  const c = document.createElement('canvas'); c.width = 40; c.height = 30; document.body.appendChild(c);
  const x = c.getContext('2d'); x.fillRect(0, 0, 5, 5); const id = x.getImageData(0, 0, 4, 4); x.putImageData(id, 1, 1); x.createImageData(id); c.toDataURL();
  const gl = document.createElement('canvas').getContext('webgl'); gl.clear(gl.COLOR_BUFFER_BIT); gl.getParameter(gl.VIEWPORT); gl.drawingBufferWidth; const px = new Uint8Array(4); gl.readPixels(0, 0, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, px);
  const oc = new OffscreenCanvas(8, 8); const og = oc.getContext('webgl'); og.clear(og.COLOR_BUFFER_BIT); og.drawingBufferHeight; const oc2 = new OffscreenCanvas(8, 8).getContext('2d'); oc2.fillRect(0, 0, 2, 2); oc2.getImageData(0, 0, 2, 2);
  const b = new Blob(['1'], { type: 'text/javascript' }); const u = URL.createObjectURL(b); const wk = new Worker(u); wk.terminate(); URL.revokeObjectURL(u);
  const dv = document.createElement('div'); dv.addEventListener('click', () => {}); document.addEventListener('x', () => {}); window.addEventListener('y', () => {});
  const s = document.createElement('script'); s.textContent = 'window.__ran=1'; document.head.appendChild(s);
  const f = document.createElement('iframe'); document.body.appendChild(f); f.contentWindow.document;
  getComputedStyle(dv); dv.getBoundingClientRect(); document.body.appendChild(dv); dv.innerHTML = '<b>x</b>'; dv.cloneNode(true); dv.querySelector('b');
  new Image().src = 'data:image/gif;base64,R0lGODlhAQABAAAAACw='; JSON.stringify(navigator.userAgentData); 
  customElements.define('x-reent', class extends HTMLElement {}); document.body.appendChild(document.createElement('x-reent'));
  const out = document.createElement('output'); out.textContent = 'v'; document.body.appendChild(out); out.value; out.defaultValue;
  new StaticRange({ startContainer: dv, startOffset: 0, endContainer: dv, endOffset: 0 });
  const s2 = document.createElement('script'); s2.appendChild(document.createTextNode('window.__ran2=1')); document.body.appendChild(s2); s2.text;
  const fr = document.createDocumentFragment(); fr.appendChild(document.createElement('span')); document.body.appendChild(fr);
  document.body.insertAdjacentHTML('beforeend', '<i>y</i>'); dv.replaceWith(document.createElement('p'));
  const st = document.createElement('style'); st.textContent = '.q{color:red}'; document.head.appendChild(st);
  for (let i = 0; i < 3; i++) { const ss = document.styleSheets; ss.length; for (const sh of ss) { sh.cssRules.length; sh.ownerNode; } } st.sheet; st.textContent = '.q{color:blue}'; document.styleSheets[0].cssRules[0].cssText;
  const rg = document.createRange(); const rd = document.createElement('div'); rd.innerHTML = '<b>a</b>c'; document.body.appendChild(rd); rg.selectNodeContents(rd); rg.toString(); rg.setStart(rd.firstChild, 0); rg.setEnd(rd.lastChild, 1); rg.collapse(true); rg.cloneRange(); rg.compareBoundaryPoints(Range.START_TO_START, rg.cloneRange());
  const an = document.createElement('a'); an.innerHTML = 'x<b>y</b>'; document.body.appendChild(an); an.text; an.text = 'z';
  await new Promise((r) => setTimeout(r, 300));
  return [...nested.entries()].map(([k, v]) => k + ' x' + v).sort();
})()
