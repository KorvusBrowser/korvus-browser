// Trusted Types ENFORCED at the sinks: the document's CSP says `require-trusted-types-for 'script'` (a header, a
// report-only header or a <head> meta -- the runner's choice), and `globalThis.__case` picks the default policy:
//   log      every create* logs (input|type|sink) and answers input + '#' (createScript: the input, unchanged)
//   none     no default policy
//   partial  a default policy with createScript only
//   nul      every create* answers null
//   thr      every create* throws a TypeError('pol')
//   child    no default policy here; an about:blank child creates ITS default policy (an interactive-widget challenge frame's sequence)
//   names    `trusted-types a default` in the header: which names createPolicy takes
//   dupes    no CSP: a second `default` policy, a second `x`
//   evalmsg  a CSP without 'unsafe-eval' (the runner's choice of directive): the EvalError's message
//   srcdoc   an interactive-widget challenge frame's shape: a srcdoc frame holding its own meta policy (`script-src 'nonce-n'`)
//            and a default policy, `eval('this')` called on its window from here (a PROMISE of the rows)
// Every row: [label, what the policy saw, the effect or the error].
(() => {
  const R = [];
  const log = [];
  const take = () => { const l = log.join(' ; '); log.length = 0; return l; };
  const err = (e) => (e && e.name) + ': ' + (e && e.message);
  const row = (label, f) => {
    let effect;
    try { effect = f(); } catch (e) { effect = 'threw ' + err(e); }
    R.push([label, take(), typeof effect === 'string' ? effect : JSON.stringify(effect)]);
  };
  const mk = (tt, kind) => {
    const L = (name) => function (s, type, sink) { log.push(name + '(' + [s, type, sink].join('|') + ')'); return s; };
    if (kind === 'log') return tt.createPolicy('default', {
      createHTML(s, type, sink) { log.push('createHTML(' + [s, type, sink].join('|') + ')'); return s + '#'; },
      createScript: L('createScript'),
      createScriptURL(s, type, sink) { log.push('createScriptURL(' + [s, type, sink].join('|') + ')'); return s + '#'; },
    });
    if (kind === 'partial') return tt.createPolicy('default', { createScript: L('createScript') });
    if (kind === 'nul') return tt.createPolicy('default', {
      createHTML: (s) => { log.push('createHTML'); return null; }, createScript: (s) => { log.push('createScript'); return null; },
      createScriptURL: (s) => { log.push('createScriptURL'); return null; } });
    if (kind === 'thr') return tt.createPolicy('default', {
      createHTML: () => { log.push('createHTML'); throw new TypeError('pol'); },
      createScript: () => { log.push('createScript'); throw new TypeError('pol'); },
      createScriptURL: () => { log.push('createScriptURL'); throw new TypeError('pol'); } });
    return null;
  };
  const C = globalThis.__case;
  if (C === 'dupes') {
    row('createPolicy(default)', () => trustedTypes.createPolicy('default', {}).name);
    row('createPolicy(default) again', () => trustedTypes.createPolicy('default', {}).name);
    row('createPolicy(x)', () => trustedTypes.createPolicy('x', {}).name);
    row('createPolicy(x) again', () => trustedTypes.createPolicy('x', {}).name);
    return R;
  }
  if (C === 'evalmsg') {
    row('eval', () => eval('1+1'));
    row('new Function', () => new Function('return 1')());
    row('setTimeout(string)', () => setTimeout('0', 0));
    return R;
  }
  if (C === 'srcdoc') {
    const p = trustedTypes.createPolicy('p', { createHTML: (s) => s });
    const f = document.createElement('iframe');
    f.style.display = 'none';
    return new Promise((done) => {
      f.onload = () => {
        const w = f.contentWindow;
        row('srcdoc: its default policy', () => w.trustedTypes.defaultPolicy ? w.trustedTypes.defaultPolicy.name : 'none');
        row('srcdoc: eval(this)', () => w.eval('this') === w ? 'its window' : 'other');
        row('srcdoc: Function', () => new w.Function('return 1')());
        row('srcdoc: setTimeout(string)', () => w.setTimeout('0', 0));
        row('srcdoc: div.innerHTML', () => { const x = w.document.createElement('div'); x.innerHTML = '<b>s</b>'; return x.innerHTML; });
        f.remove();
        done(R);
      };
      f.srcdoc = p.createHTML('<html><meta http-equiv="content-security-policy" content="script-src \'nonce-n\'"><script nonce="n">' +
        'trustedTypes.createPolicy("default", { createScript(s, t, k) { parent.__log.push("createScript(" + [s, t, k].join("|") + ")"); return s; },' +
        ' createHTML(s, t, k) { parent.__log.push("createHTML(" + [s, t, k].join("|") + ")"); return s + "#"; } });<\/script>');
      globalThis.__log = log;
      document.body.appendChild(f);
    });
  }
  if (C === 'names') {
    const tt = trustedTypes;
    row('createPolicy(a)', () => tt.createPolicy('a', {}).name);
    row('createPolicy(a) again', () => tt.createPolicy('a', {}).name);
    row('createPolicy(b)', () => tt.createPolicy('b', {}).name);
    row('createPolicy(default)', () => tt.createPolicy('default', {}).name);
    row('createPolicy(default) again', () => tt.createPolicy('default', {}).name);
    return R;
  }
  if (C === 'child') {
    const f = document.createElement('iframe');
    f.width = 0; f.height = 0; f.style.display = 'none';
    document.body.appendChild(f);
    const w = f.contentWindow, d = f.contentDocument;
    row('child: createPolicy(default)', () => w.trustedTypes.createPolicy('default', {
      createHTML(s, type, sink) { log.push('createHTML(' + [s, type, sink].join('|') + ')'); return ''; },
      createScript(s, type, sink) { log.push('createScript(' + [s, type, sink].join('|') + ')'); return ''; },
      createScriptURL(s, type, sink) { log.push('createScriptURL(' + [s, type, sink].join('|') + ')'); return 'about:blank'; },
    }).name);
    row('child div.innerHTML', () => { const x = d.createElement('div'); x.innerHTML = '<b>t</b>'; return x.innerHTML; });
    row('child eval', () => w.eval('0'));
    row('child script.src', () => { const s = d.createElement('script'); s.src = 'data:,0'; return s.getAttribute('src'); });
    row('child setTimeout(string)', () => typeof w.setTimeout('0', 0));
    row('child setAttribute(onclick)', () => { const x = d.createElement('div'); x.setAttribute('onclick', 'x()'); return x.getAttribute('onclick'); });
    row('parent div.innerHTML (no parent default policy)', () => { const x = document.createElement('div'); x.innerHTML = '<b>p</b>'; return x.innerHTML; });
    row('parent-made div moved into the child', () => { const x = document.createElement('div'); d.body.appendChild(x); x.innerHTML = '<b>m</b>'; return x.innerHTML; });
    row('child-made div moved into the parent', () => { const x = d.createElement('div'); document.body.appendChild(x); x.innerHTML = '<b>n</b>'; const v = x.innerHTML; x.remove(); return v; });
    row('child Element.prototype.innerHTML setter on a parent div', () => { const x = document.createElement('div'); Object.getOwnPropertyDescriptor(w.Element.prototype, 'innerHTML').set.call(x, '<b>q</b>'); return x.innerHTML; });
    row('srcdoc on the parent (no parent default policy)', () => { const g = document.createElement('iframe'); g.srcdoc = 'x'; return g.getAttribute('srcdoc'); });
    f.remove();
    return R;
  }
  const pol = mk(trustedTypes, C);
  row('default policy', () => pol ? pol.name : 'none');
  const div = () => document.createElement('div');
  row('innerHTML', () => { const d = div(); d.innerHTML = '<b>a</b>'; return d.innerHTML; });
  row('innerHTML = null', () => { const d = div(); d.innerHTML = null; return d.innerHTML; });
  row('outerHTML', () => { const d = div(); const c = d.appendChild(div()); c.outerHTML = '<p>o</p>'; return d.innerHTML; });
  row('insertAdjacentHTML', () => { const d = div(); d.insertAdjacentHTML('beforeend', '<u>x</u>'); return d.innerHTML; });
  row('setHTMLUnsafe', () => { const d = div(); d.setHTMLUnsafe('<s>u</s>'); return d.innerHTML; });
  row('shadowRoot.innerHTML', () => { const d = div(); const sr = d.attachShadow({ mode: 'open' }); sr.innerHTML = '<em>r</em>'; return sr.innerHTML; });
  row('shadowRoot.setHTMLUnsafe', () => { const d = div(); const sr = d.attachShadow({ mode: 'open' }); sr.setHTMLUnsafe('<em>s</em>'); return sr.innerHTML; });
  row('iframe.srcdoc', () => { const f = document.createElement('iframe'); f.srcdoc = '<p>d</p>'; return f.getAttribute('srcdoc'); });
  row('Range.createContextualFragment', () => { const d = div(); document.body.append(d); const r = document.createRange(); r.selectNodeContents(d); const fr = r.createContextualFragment('<a>f</a>'); d.remove(); return fr.childNodes.length + ':' + (fr.textContent); });
  row('DOMParser.parseFromString', () => new DOMParser().parseFromString('<p>z</p>', 'text/html').body.innerHTML);
  row('Document.parseHTMLUnsafe', () => Document.parseHTMLUnsafe('<p>q</p>').body.innerHTML);
  row('document.write (a new document)', () => { const doc = document.implementation.createHTMLDocument(''); doc.open(); doc.write('<p>w</p>'); doc.close(); return doc.body ? doc.body.innerHTML : 'nobody'; });
  row('script.src', () => { const s = document.createElement('script'); s.src = '/nothing.js'; return s.getAttribute('src'); });
  row('script.text', () => { const s = document.createElement('script'); s.text = 'var t1 = 1;'; return s.text; });
  row('script.textContent', () => { const s = document.createElement('script'); s.textContent = 'var t2 = 2;'; return s.textContent; });
  row('script.innerText', () => { const s = document.createElement('script'); s.innerText = 'var t3 = 3;'; return s.textContent; });
  row('script.append(string)', () => { const s = document.createElement('script'); s.append('var t4 = 4;'); return s.textContent; });
  row('script.appendChild(text)', () => { const s = document.createElement('script'); s.appendChild(document.createTextNode('var t5 = 5;')); return s.textContent; });
  row('div.textContent', () => { const d = div(); d.textContent = 'tc'; return d.textContent; });
  row('setAttribute(script, src)', () => { const s = document.createElement('script'); s.setAttribute('src', '/sa.js'); return s.getAttribute('src'); });
  row('setAttribute(iframe, srcdoc)', () => { const f = document.createElement('iframe'); f.setAttribute('srcdoc', '<i>sd</i>'); return f.getAttribute('srcdoc'); });
  row('setAttribute(div, onclick)', () => { const d = div(); d.setAttribute('onclick', 'go()'); return d.getAttribute('onclick'); });
  row('setAttribute(div, data-x)', () => { const d = div(); d.setAttribute('data-x', 'dx'); return d.getAttribute('data-x'); });
  row('setAttributeNS(null, script, src)', () => { const s = document.createElement('script'); s.setAttributeNS(null, 'src', '/ns.js'); return s.getAttribute('src'); });
  row('Attr.value (script src)', () => { const s = document.createElement('script'); s.setAttribute('id', 'i'); const a = document.createAttribute('src'); s.setAttributeNode(a); a.value = '/av.js'; return s.getAttribute('src'); });
  row('Attr.value (an attached script src)', () => { const q = globalThis.__q || (globalThis.__q = trustedTypes.createPolicy('q', { createScriptURL: (u) => u })); const s = document.createElement('script'); s.setAttribute('src', q.createScriptURL('/q.js')); const a = s.getAttributeNode('src'); a.value = '/av2.js'; return s.getAttribute('src'); });
  row('embed.src', () => { const e = document.createElement('embed'); e.src = '/e.bin'; return e.getAttribute('src'); });
  row('object.data', () => { const o = document.createElement('object'); o.data = '/o.bin'; return o.getAttribute('data'); });
  row('object.codeBase', () => { const o = document.createElement('object'); o.codeBase = '/cb/'; return o.getAttribute('codebase'); });
  row('svg script href.baseVal', () => { const s = document.createElementNS('http://www.w3.org/2000/svg', 'script'); s.href.baseVal = '/svg.js'; return s.getAttribute('href'); });
  row('new Worker(string)', () => { const w = new Worker('/w.js'); w.terminate(); return 'made'; });
  row('setTimeout(string)', () => { const id = setTimeout('window.__t = 1', 0); clearTimeout(id); return typeof id; });
  row('eval', () => eval('1+1'));
  return R;
})()
