// PARITY P11a -- the window surface a bot check reads.
//
// Evaluated UNMODIFIED by `tests/window_surface_p11a_live.rs` and by the four stock browsers over CDP
// (`probe-chrome.py`), on `same + "page.html"`, with a second origin `xo` serving the same routes
// (`probe-server.py`). `section` picks what to print. Every section prints one `key => value` line per
// fact, so a difference is one line.
async (same, xo, section) => {
  const out = [];
  const log = (k, v) => out.push(k + ' => ' + v);
  const T = async (k, f) => { try { log(k, await f()); } catch (e) { log(k, 'THROW ' + e.name + ': ' + e.message); } };
  const fn = (f) => (typeof f === 'function' ? JSON.stringify(f.name) + '/' + f.length : String(f));
  const shape = (d) => {
    if (!d) return 'absent';
    const flags = ' e=' + d.enumerable + ' c=' + d.configurable;
    if ('value' in d) {
      const v = d.value;
      const t = v === null ? 'null' : typeof v;
      return 'data w=' + d.writable + flags + ' ' + t + (t === 'function' ? ' ' + fn(v) : '');
    }
    return 'accessor get=' + fn(d.get) + ' set=' + fn(d.set) + flags;
  };
  const keyName = (k) => (typeof k === 'symbol' ? '[' + k.description + ']' : k);
  const own = (label, o, filter) => {
    const keys = Reflect.ownKeys(o).filter((k) => !filter || filter(k));
    const lines = keys.map((k) => label + '.' + keyName(k) + ' => ' + shape(Object.getOwnPropertyDescriptor(o, k)));
    lines.sort();
    out.push(...lines);
  };
  const frame = (src) => new Promise((resolve) => {
    const f = document.createElement('iframe');
    let done = false;
    const fin = () => { if (!done) { done = true; resolve(f); } };
    f.onload = fin;
    setTimeout(fin, 5000);
    f.src = src;
    document.body.appendChild(f);
  });
  const tick = (ms) => new Promise((r) => setTimeout(r, ms));

  // Every own property of the window, its prototype, the named-properties object, the document, the
  // location and the ViewTransition prototype, as a descriptor SHAPE (never a value).
  if (section === 'desc') {
    own('window', window);
    own('Window.prototype', Window.prototype);
    own('WindowProperties', Object.getPrototypeOf(Window.prototype));
    own('document', document);
    own('location', location);
    own('Location.prototype', Location.prototype);
    log('Document.prototype.startViewTransition', shape(Object.getOwnPropertyDescriptor(Document.prototype, 'startViewTransition')));
    if (typeof ViewTransition === 'function') own('ViewTransition.prototype', ViewTransition.prototype);
    return out.join('\n');
  }

  // Assign 7 to every own accessor of the window in sloppy mode (this probe is not strict) and report
  // what reading it back gives and what the descriptor became. `location` navigates and is skipped;
  // `window.location = url` is the `nav` table's.
  if (section === 'replace') {
    const names = Reflect.ownKeys(window).filter((k) => typeof k === 'string' && k !== 'location' && Object.getOwnPropertyDescriptor(window, k).get);
    names.sort();
    const before = {};
    for (const n of names) before[n] = window[n];
    for (const n of names) {
      let r;
      try { window[n] = 7; r = 'ok'; } catch (e) { r = 'THROW ' + e.name + ': ' + e.message; }
      const after = window[n];
      const same7 = after === 7 ? '7' : after === before[n] ? 'unchanged' : after === null ? 'null' : typeof after + (typeof after === 'string' ? ' ' + JSON.stringify(after) : '');
      const d = Object.getOwnPropertyDescriptor(window, n);
      log('window.' + n + ' = 7', r + ' reads ' + same7 + ' now ' + (d ? ('value' in d ? 'data w=' + d.writable + ' e=' + d.enumerable + ' c=' + d.configurable : 'accessor') : 'absent'));
    }
    return out.join('\n');
  }

  // The receiver checks of the window's own accessors: a foreign object, `undefined`, and a setter
  // called on a foreign object.
  if (section === 'recv') {
    const names = ['document', 'window', 'self', 'top', 'parent', 'frames', 'location', 'navigator', 'screen', 'performance', 'history', 'localStorage', 'sessionStorage', 'customElements', 'cookieStore', 'origin', 'opener', 'name', 'crypto', 'caches', 'indexedDB'];
    const show = (v) => (v === window ? 'window' : v === null ? 'null' : typeof v === 'object' ? Object.prototype.toString.call(v) : typeof v);
    for (const n of names) {
      const d = Object.getOwnPropertyDescriptor(window, n);
      if (!d || !d.get) { log(n, 'not an accessor'); continue; }
      await T(n + ' get.call({})', () => show(d.get.call({})));
      await T(n + ' get.call(undefined)', () => show(d.get.call(undefined)) + ' same=' + (d.get.call(undefined) === window[n]));
      await T(n + ' get.call(Object.create(Window.prototype))', () => show(d.get.call(Object.create(Window.prototype))));
      await T(n + ' get identity', () => (Object.getOwnPropertyDescriptor(window, n).get === d.get));
      if (d.set) await T(n + ' set.call({}, 7)', () => { d.set.call({}, 7); return 'no throw'; });
    }
    await T('strict self = 7', () => { 'use strict'; const d0 = Object.getOwnPropertyDescriptor(window, 'navigator'); try { (function () { 'use strict'; window.navigator = 7; })(); return 'no throw'; } catch (e) { return 'THROW ' + e.name + ': ' + e.message; } });
    await T('strict document = 7', () => { try { (function () { 'use strict'; window.document = 7; })(); return 'no throw'; } catch (e) { return 'THROW ' + e.name + ': ' + e.message; } });
    await T('strict top = 7', () => { try { (function () { 'use strict'; window.top = 7; })(); return 'no throw'; } catch (e) { return 'THROW ' + e.name + ': ' + e.message; } });
    await T('delete window.navigator', () => delete window.navigator);
    await T('navigator after delete', () => typeof navigator);
    await T('delete window.document', () => delete window.document);
    await T('opener = null then', () => { window.opener = null; const d = Object.getOwnPropertyDescriptor(window, 'opener'); return 'value' in d ? 'data ' + d.value : 'accessor ' + window.opener; });
    await T('opener = {} then', () => { const o = {}; window.opener = o; const d = Object.getOwnPropertyDescriptor(window, 'opener'); return ('value' in d ? 'data w=' + d.writable + ' e=' + d.enumerable + ' c=' + d.configurable : 'accessor') + ' same=' + (window.opener === o); });
    return out.join('\n');
  }

  // `document.startViewTransition`: what it returns and the order its promises settle in.
  if (section === 'vt') {
    const order = [];
    const rec = (k) => (v) => order.push(k + (v === undefined ? '' : ' ' + (v && v.name ? v.name : String(v))));
    const run = async (label, cb, pre) => {
      order.length = 0;
      let vt;
      try { vt = document.startViewTransition(cb); } catch (e) { log(label, 'THROW ' + e.name + ': ' + e.message); return; }
      order.push('returned');
      if (pre) pre(vt);
      log(label + ' tag', Object.prototype.toString.call(vt));
      log(label + ' types', vt && vt.types && typeof vt.types + ' ' + Object.prototype.toString.call(vt.types) + ' size=' + vt.types.size);
      log(label + ' transitionRoot', vt && vt.transitionRoot === document.documentElement);
      const ps = ['updateCallbackDone', 'ready', 'finished'].map((k) => vt[k].then(rec(k + ' fulfilled'), rec(k + ' rejected')));
      Promise.resolve().then(() => order.push('microtask'));
      await Promise.all(ps);
      await tick(50);
      log(label + ' order', order.join(', '));
    };
    await run('no callback', undefined);
    await run('sync callback', () => { order.push('callback'); });
    await run('async callback', () => { order.push('callback'); return tick(20).then(() => order.push('callback resolved')); });
    await run('throwing callback', () => { order.push('callback'); throw new TypeError('boom'); });
    await run('skipTransition', () => { order.push('callback'); }, (vt) => { vt.skipTransition(); order.push('skipped'); });
    await run('options object', { update: () => { order.push('callback'); }, types: ['a', 'b'] });
    await T('non-function argument', () => { const v = document.startViewTransition(5); return Object.prototype.toString.call(v); });
    await T('activeViewTransition', () => typeof document.activeViewTransition);
    return out.join('\n');
  }

  // A cross-origin <iframe>: which members its WindowProxy and Location answer, and the text of each
  // refusal. The origin in a message is replaced by `<same>` / `<xo>` so the line is port-free.
  if (section === 'xo') {
    const f = await frame(xo + 'child.html');
    await tick(200);
    const w = f.contentWindow;
    const scrub = (s) => String(s).split(same.slice(0, -1)).join('<same>').split(xo.slice(0, -1)).join('<xo>');
    const E = (k, g) => { try { const v = g(); log(k, 'ok ' + (v === null ? 'null' : ['string', 'boolean', 'number'].includes(typeof v) ? typeof v + ' ' + scrub(v) : typeof v)); } catch (e) { log(k, 'THROW ' + e.name + ': ' + scrub(e.message)); } };
    log('contentDocument', f.contentDocument);
    E('typeof contentWindow', () => w);
    const names = Reflect.ownKeys(window).filter((k) => typeof k === 'string');
    names.push('then', 'x-not-a-member', '0', 'JSON', 'Object', 'Array');
    const allowed = [];
    const refusals = {};
    for (const n of [...new Set(names)]) {
      try {
        const v = w[n];
        allowed.push(n + ':' + (v === null ? 'null' : typeof v));
      } catch (e) {
        const m = e.name + ': ' + scrub(e.message).split("'" + n + "'").join("'%'");
        refusals[m] = (refusals[m] || 0) + 1;
      }
    }
    allowed.sort();
    log('get allowed', allowed.join(' '));
    for (const m of Object.keys(refusals).sort()) log('get refused', m);
    for (const s of [Symbol.toStringTag, Symbol.hasInstance, Symbol.isConcatSpreadable, Symbol.iterator]) E('get ' + s.description, () => w[s]);
    E('getOwnPropertyNames', () => Object.getOwnPropertyNames(w).sort().join(' '));
    try { log('getOwnPropertyNames list', Object.getOwnPropertyNames(w).join(' ')); } catch (e) { log('getOwnPropertyNames list', 'THROW ' + e.name); }
    E('keys', () => Object.keys(w).join(' '));
    E('getPrototypeOf', () => Object.getPrototypeOf(w));
    E('toString.call', () => Object.prototype.toString.call(w));
    for (const n of ['postMessage', 'location', 'close', 'closed', 'window', 'self', 'top', 'parent', 'frames', 'opener', 'length', 'blur', 'focus', 'document', 'name']) {
      try { log('desc ' + n, shape(Object.getOwnPropertyDescriptor(w, n))); } catch (e) { log('desc ' + n, 'THROW ' + e.name + ': ' + scrub(e.message)); }
    }
    E('window === w', () => String(w.window === w) + ' self ' + (w.self === w) + ' frames ' + (w.frames === w) + ' top ' + (w.top === window) + ' parent ' + (w.parent === window) + ' length ' + w.length + ' closed ' + w.closed + ' opener ' + w.opener);
    E('set x', () => { w.x = 1; return 'no throw'; });
    E('set name', () => { w.name = 'n'; return 'no throw'; });
    E('defineProperty', () => { Object.defineProperty(w, 'y', { value: 1 }); return 'no throw'; });
    E('delete document', () => delete w.document);
    E('in document', () => 'document' in w);
    E('in postMessage', () => 'postMessage' in w);
    E('setPrototypeOf', () => Object.setPrototypeOf(w, {}));
    E('preventExtensions', () => Object.preventExtensions(w));
    E('isExtensible', () => Object.isExtensible(w));
    E('postMessage call', () => w.postMessage('hi', '*'));
    E('postMessage same', () => w.postMessage === w.postMessage);
    E('focus call', () => w.focus());
    const L = w.location;
    E('typeof location', () => L);
    for (const n of ['href', 'protocol', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'origin', 'assign', 'reload', 'replace', 'toString', 'ancestorOrigins', 'then']) E('location.' + n, () => L[n]);
    E('location getOwnPropertyNames', () => Object.getOwnPropertyNames(L).sort().join(' '));
    E('location.replace same', () => L.replace === L.replace);
    E('location toString.call', () => Object.prototype.toString.call(L));
    E('location set pathname', () => { L.pathname = '/x'; return 'no throw'; });
    E('location set href', () => { L.href = xo + 'child.html#moved'; return 'no throw'; });
    E('w.location = url', () => { w.location = xo + 'child.html#moved2'; return 'no throw'; });
    return out.join('\n');
  }

  // A same-origin <iframe>: realm identity. One V8 context would make every line `true`.
  if (section === 'so') {
    const f = await frame(same + 'child.html');
    await tick(100);
    const w = f.contentWindow;
    log('contentWindow.JSON === JSON', w.JSON === JSON);
    log('contentWindow.Array === Array', w.Array === Array);
    log('contentWindow.Object.prototype === Object.prototype', w.Object.prototype === Object.prototype);
    log('contentWindow.Node === Node', w.Node === Node);
    log('contentWindow.document instanceof Document', w.document instanceof Document);
    log('contentWindow.document instanceof contentWindow.Document', w.document instanceof w.Document);
    log('[] instanceof contentWindow.Array', [] instanceof w.Array);
    log('contentWindow.eval("[]") instanceof Array', w.eval('[]') instanceof Array);
    log('Array.isArray(contentWindow.eval("[]"))', Array.isArray(w.eval('[]')));
    log('contentWindow.Function("return this")() === contentWindow', w.Function('return this')() === w);
    return out.join('\n');
  }

  // `location.hash = ...`: a same-document navigation. What changes synchronously, and the order the
  // events arrive in.
  if (section === 'fragment') {
    await T('new PopStateEvent("p")', () => { const e = new PopStateEvent('p'); return [e.state, e.hasUAVisualTransition, e.isTrusted, e.bubbles, Object.prototype.toString.call(e)].join(' '); });
    await T('new PopStateEvent("p", {state: 5})', () => new PopStateEvent('p', { state: 5 }).state);
    await T('new PopStateEvent("p", {state: undefined})', () => String(new PopStateEvent('p', { state: undefined }).state));
    await T('new HashChangeEvent("h", {oldURL: 1, newURL: "x"})', () => { const e = new HashChangeEvent('h', { oldURL: 1, newURL: 'x' }); return JSON.stringify([e.oldURL, e.newURL]); });
    await T('new HashChangeEvent("h")', () => JSON.stringify([new HashChangeEvent('h').oldURL, new HashChangeEvent('h').newURL]));
    await T('new PopStateEvent()', () => new PopStateEvent());
    await T('PopStateEvent("p")', () => PopStateEvent('p'));
    await T('new HashChangeEvent("h", 5)', () => new HashChangeEvent('h', 5));
    const order = [];
    const h0 = history.length;
    const onPop = (e) => order.push('popstate state=' + JSON.stringify(e.state) + ' href=' + location.hash + ' trusted=' + e.isTrusted + ' bubbles=' + e.bubbles + ' cancelable=' + e.cancelable);
    const onHash = (e) => order.push('hashchange old=' + e.oldURL.slice(e.oldURL.lastIndexOf('/')) + ' new=' + e.newURL.slice(e.newURL.lastIndexOf('/')) + ' ' + Object.prototype.toString.call(e) + ' trusted=' + e.isTrusted + ' bubbles=' + e.bubbles + ' cancelable=' + e.cancelable);
    window.addEventListener('popstate', onPop);
    window.addEventListener('hashchange', onHash);
    // A history TRAVERSAL crosses to the browser process and back, so where it lands against a
    // `setTimeout(0)` is a race in the browsers themselves (Edge and Opera each put one traversal after
    // it in one recording); the traversal steps mark a 50 ms timer instead, which none reaches.
    const step = async (label, f, timerMs = 0) => {
      order.length = 0;
      f();
      order.push('sync hash=' + location.hash + ' dh=' + (history.length - h0));
      Promise.resolve().then(() => order.push('microtask'));
      setTimeout(() => order.push('timeout' + timerMs), timerMs);
      await tick(150);
      log(label, order.join(', '));
    };
    await step('location.hash = "a"', () => { location.hash = 'a'; });
    await step('location.hash = "#b"', () => { location.hash = '#b'; });
    await step('location.hash = "#b" again', () => { location.hash = '#b'; });
    await step('location.href = "#c"', () => { location.href = '#c'; });
    await step('location.assign("#d")', () => { location.assign('#d'); });
    await step('location.replace("#e")', () => { location.replace('#e'); });
    await step('location.hash = ""', () => { location.hash = ''; });
    await step('history.back()', () => { history.back(); }, 50);
    await step('history.pushState({n: 1}, "", "?s=1#e")', () => { history.pushState({ n: 1 }, '', '?s=1#e'); });
    await step('history.pushState({n: 2}, "", "#q")', () => { history.pushState({ n: 2 }, '', '#q'); });
    await step('history.replaceState({n: 3}, "")', () => { history.replaceState({ n: 3 }, ''); });
    await step('history.back() after pushes', () => { history.back(); }, 50);
    await step('history.forward()', () => { history.forward(); }, 50);
    await step('history.go(-2)', () => { history.go(-2); }, 50);
    await step('history.go(10)', () => { history.go(10); }, 50);
    await step('history.state', () => { order.push('state=' + JSON.stringify(history.state) + ' search=' + location.search); });
    log('final', location.href.slice(location.href.indexOf('/page.html')) + ' dh=' + (history.length - h0));
    window.removeEventListener('popstate', onPop);
    window.removeEventListener('hashchange', onHash);
    return out.join('\n');
  }

  // The argument checks and read-side shape of Location, without navigating.
  if (section === 'location') {
    const L = location;
    await T('String(location)', () => String(L) === L.href);
    await T('location.valueOf() === location', () => L.valueOf() === L);
    await T('location.ancestorOrigins', () => Object.prototype.toString.call(L.ancestorOrigins) + ' ' + L.ancestorOrigins.length);
    await T('location.assign()', () => L.assign());
    await T('location.replace()', () => L.replace());
    await T('location.assign("http://[")', () => L.assign('http://['));
    await T('location.href = "http://["', () => { L.href = 'http://['; return 'no throw'; });
    for (const u of ['http://x:99999/', '//[', 'http://[::1', 'https://', 'http://%zz/']) {
      await T('location.replace(' + JSON.stringify(u) + ')', () => { L.replace(u); return 'no throw'; });
    }
    await T('location.assign.length', () => L.assign.length + ' ' + L.replace.length + ' ' + L.reload.length + ' ' + L.toString.length);
    await T('location.valueOf === Object.prototype.valueOf', () => L.valueOf === Object.prototype.valueOf);
    await T('location[Symbol.toPrimitive]', () => typeof L[Symbol.toPrimitive]);
    await T('ancestorOrigins same object', () => L.ancestorOrigins === L.ancestorOrigins);
    await T('location.port', () => typeof L.port + ' ' + (L.port === location.host.split(':')[1]));
    await T('Location.prototype own', () => Object.getOwnPropertyNames(Location.prototype).join(' '));
    await T('new Location()', () => new Location());
    await T('Location()', () => Location());
    await T('location instanceof Location', () => location instanceof Location);
    await T('Object.keys(location)', () => Object.keys(location).join(' '));
    await T('location.protocol = ""', () => { L.protocol = ''; return 'no throw'; });
    await T('location.protocol = "ht tp"', () => { L.protocol = 'ht tp'; return 'no throw'; });
    await T('location.protocol = "data"', () => { L.protocol = 'data'; return 'no throw ' + L.protocol; });
    await T('set pathname on receiver {}', () => { Object.getOwnPropertyDescriptor(L, 'pathname').set.call({}, '/x'); return 'no throw'; });
    await T('get href on receiver {}', () => Object.getOwnPropertyDescriptor(L, 'href').get.call({}));
    await T('assign.call({})', () => L.assign.call({}, '#z'));
    await T('Object.defineProperty(location, "href", {value:1})', () => { Object.defineProperty(L, 'href', { value: 1 }); return 'no throw'; });
    await T('delete location.href', () => delete L.href);
    await T('location.x = 1', () => { L.x = 1; return L.x; });
    await T('Object.setPrototypeOf(location, null)', () => { Object.setPrototypeOf(L, null); return 'no throw'; });
    await T('document.location === location', () => document.location === L);
    await T('window.location === location', () => window.location === L);
    return out.join('\n');
  }

  return 'unknown section ' + section;
}
