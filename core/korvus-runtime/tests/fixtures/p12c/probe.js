// PARITY P12c -- IntersectionObserver and ResizeObserver deliver, in the browsers' order.
//
// Evaluated UNMODIFIED by `tests/observer_delivery_live.rs` and by stock Chrome over CDP (`record.py`),
// section by section and phase by phase, on an empty loopback `page.html`. A phase is one
// `Runtime.evaluate` (awaitPromise): state carries across phases on `window.__p12c`, and whatever a
// phase changes (a scroll, a style) is observed by the NEXT phase, which is how a client drives a
// page -- every command is its own turn of the page's event loop.
//
// Nothing here reads a clock. A phase that waits does so to let the page's rendering opportunities
// happen; what it prints is WHICH callbacks ran, in WHAT order, with WHAT entries -- counts and
// values, never times. Viewport-relative numbers are printed symbolically (VW, VH, VH-50) because
// the two engines' windows are different sizes and the size is not what this probe is about.
async (section, phase) => {
  const out = [];
  const log = (k, v) => out.push(k + ' => ' + v);
  const tick = (ms) => new Promise((r) => setTimeout(r, ms));
  const settle = () => tick(300);
  const S = (window.__p12c = window.__p12c || {});
  const de = document.documentElement;
  const vw = () => de.clientWidth;
  const vh = () => de.clientHeight;
  const num = (n) => {
    if (typeof n !== 'number') return String(n);
    const named = [['VW', vw()], ['VH', vh()], ['VH-50', vh() - 50], ['VH+3000', vh() + 3000], ['floor(VH*1.5)', Math.floor(vh() * 1.5)],
      ['VH+50', vh() + 50], ['VW-20', vw() - 20], ['VH-20', vh() - 20], ['-SY', -scrollY], ['3000-SY', 3000 - scrollY]];
    for (const [k, v] of named) if (Math.abs(n - v) < 0.005 && Math.abs(n) > 300) return k;
    return String(Math.round(n * 100) / 100);
  };
  const rect = (r) => (r === null ? 'null' : r === undefined ? 'undefined' : [r.x, r.y, r.width, r.height].map(num).join(','));
  const T = (k, f) => { try { log(k, f()); } catch (e) { log(k, 'THROW ' + e.name + ': ' + e.message); } };
  const css = (text) => { const s = document.createElement('style'); s.textContent = text; document.head.appendChild(s); };
  const div = (id, style, parent) => {
    const d = document.createElement('div');
    d.id = id;
    if (style) d.setAttribute('style', style);
    (parent || document.body).appendChild(d);
    return d;
  };
  const name = (t) => (t === document ? '#document' : t === de ? 'html' : t === document.body ? 'body' : (t && (t.id || t.nodeName.toLowerCase())));
  const ioEntry = (e) => [name(e.target), 'is=' + e.isIntersecting, 'ratio=' + num(e.intersectionRatio),
    'b=' + rect(e.boundingClientRect), 'i=' + rect(e.intersectionRect), 'rb=' + rect(e.rootBounds),
    'time=' + (typeof e.time === 'number' && e.time > 0), 'vis=' + e.isVisible,
    'proto=' + (Object.getPrototypeOf(e) === IntersectionObserverEntry.prototype)].join(' ');
  const size = (l) => (l === undefined ? 'undefined'
    : 'len' + l.length + (l.length ? ':' + num(l[0].inlineSize) + 'x' + num(l[0].blockSize) : '') + (Object.isFrozen(l) ? ':frozen' : ':unfrozen'));
  const roEntry = (e) => [name(e.target), 'cr=' + rect(e.contentRect), 'content=' + size(e.contentBoxSize),
    'border=' + size(e.borderBoxSize), 'dp=' + size(e.devicePixelContentBoxSize),
    'crproto=' + (e.contentRect && Object.getPrototypeOf(e.contentRect).constructor.name),
    'proto=' + (Object.getPrototypeOf(e) === ResizeObserverEntry.prototype)].join(' ');
  // Record one callback invocation: its entries, `this`, and the observer argument.
  const recorder = (label, calls, fmt) => function (entries, obs) {
    calls.push(label + ' call' + (calls.length + 1) + ' n=' + entries.length + ' array=' + Array.isArray(entries)
      + ' this=' + (this === obs) + ' | ' + entries.map(fmt).join(' ; '));
  };
  const dump = (calls, from) => { for (let i = from || 0; i < calls.length; i++) out.push(calls[i]); log('calls', calls.length); };
  // The order in which DIFFERENT intersection observers are called back is not stable in Chrome 150
  // (three runs of `io-geom` gave three orders), so a section with several of them prints its
  // callbacks sorted, without their sequence numbers.
  const dumpUnordered = (calls, from) => {
    out.push(...calls.slice(from || 0).map((c) => c.replace(/ call\d+ /, ' ')).sort());
    log('calls', calls.length);
  };

  if (section === 'io-basic') {
    if (phase === 0) {
      css('html,body{margin:0;padding:0}');
      S.t = div('t', 'position:absolute;left:0;top:0;width:200px;height:100px');
      S.far = div('far', 'position:absolute;left:0;top:3000px;width:200px;height:100px');
      S.calls = [];
      S.io = new IntersectionObserver(recorder('io', S.calls, ioEntry));
      S.io.observe(S.t);
      S.io.observe(S.far);
      S.io.observe(S.t); // a second observe of the same target is not a second registration
      log('sync calls', S.calls.length);
      log('sync takeRecords', S.io.takeRecords().length);
      await settle();
      dump(S.calls);
      return out.join('\n');
    }
    if (phase === 1) {
      await settle();
      log('no change, calls', S.calls.length);
      S.mark = S.calls.length;
      scrollTo(0, 3000);
      log('scrolled to the bottom', scrollY === de.scrollHeight - vh());
      log('sync takeRecords after scroll', S.io.takeRecords().length);
      return out.join('\n');
    }
    if (phase === 2) {
      await settle();
      dump(S.calls, S.mark);
      S.mark = S.calls.length;
      S.io.unobserve(S.far);
      S.io.unobserve(S.far);
      scrollTo(0, 0);
      return out.join('\n');
    }
    if (phase === 3) {
      await settle();
      dump(S.calls, S.mark);
      S.mark = S.calls.length;
      S.io.disconnect();
      scrollTo(0, 3000);
      return out.join('\n');
    }
    await settle();
    dump(S.calls, S.mark);
    return out.join('\n');
  }

  // Constructor and attribute answers: no delivery involved.
  if (section === 'io-options') {
    const IO = IntersectionObserver;
    const f = () => {};
    T('thresholds default', () => new IO(f).thresholds.join(','));
    T('thresholds [1,0,0.5,0.5]', () => new IO(f, { threshold: [1, 0, 0.5, 0.5] }).thresholds.join(','));
    T('thresholds 0.25', () => new IO(f, { threshold: 0.25 }).thresholds.join(','));
    T('thresholds []', () => new IO(f, { threshold: [] }).thresholds.join(','));
    T('thresholds "0.5"', () => new IO(f, { threshold: '0.5' }).thresholds.join(','));
    T('thresholds 1.5', () => new IO(f, { threshold: 1.5 }).thresholds.join(','));
    T('thresholds -0.1', () => new IO(f, { threshold: [-0.1] }).thresholds.join(','));
    T('thresholds NaN', () => new IO(f, { threshold: NaN }).thresholds.join(','));
    T('thresholds frozen', () => { const o = new IO(f, { threshold: [0, 1] }); return o.thresholds === o.thresholds; });
    for (const m of ['', '10px', '10px 20px', '1px 2px 3px', '1px 2px 3px 4px', '5%', '  7px  ', '-10px', '0', '1em', 'bad', '1px 2px 3px 4px 5px', '10']) {
      T('rootMargin ' + JSON.stringify(m), () => new IO(f, { rootMargin: m }).rootMargin);
    }
    T('scrollMargin "5px"', () => new IO(f, { scrollMargin: '5px' }).scrollMargin);
    T('root document', () => new IO(f, { root: document }).root === document);
    T('root null', () => String(new IO(f, { root: null }).root));
    T('root body', () => name(new IO(f, { root: document.body }).root));
    T('root 5', () => String(new IO(f, { root: 5 }).root));
    T('root {}', () => String(new IO(f, { root: {} }).root));
    T('delay 100', () => new IO(f, { delay: 100 }).delay);
    T('trackVisibility true', () => new IO(f, { trackVisibility: true }).trackVisibility);
    T('trackVisibility true delay 100', () => { const o = new IO(f, { trackVisibility: true, delay: 100 }); return o.trackVisibility + ' ' + o.delay; });
    T('observe(5)', () => new IO(f).observe(5));
    T('observe()', () => new IO(f).observe());
    T('observe(text)', () => new IO(f).observe(document.createTextNode('x')));
    T('unobserve(5)', () => new IO(f).unobserve(5));
    T('takeRecords array', () => Array.isArray(new IO(f).takeRecords()));
    T('observe returns', () => String(new IO(f).observe(document.body)));
    T('observe body with root=child', () => { const c = div('rc', ''); return String(new IO(f, { root: c }).observe(document.body)); });
    return out.join('\n');
  }

  // Geometry: margins, thresholds, clips, explicit roots, targets with no box.
  if (section === 'io-geom') {
    if (phase === 0) {
      css('html,body{margin:0;padding:0} .b{position:absolute;left:0;width:200px;height:100px}');
      S.calls = [];
      const mk = (label, opts) => new IntersectionObserver(recorder(label, S.calls, ioEntry), opts);
      S.half = div('half', 'top:' + (vh() - 50) + 'px'); S.half.className = 'b';
      S.o_half = mk('half', { threshold: [0, 0.5, 1] }); S.o_half.observe(S.half);
      S.farm = div('farm', 'top:3500px'); S.farm.className = 'b';
      S.o_mar = mk('mar', { rootMargin: '0px 0px 3000px 0px' }); S.o_mar.observe(S.farm);
      S.inv = div('inv', 'top:0px'); S.inv.className = 'b';
      S.o_neg = mk('neg', { rootMargin: '-10px' }); S.o_neg.observe(S.inv);
      S.o_pct = mk('pct', { rootMargin: '0px 0px 50% 0px' });
      S.pctt = div('pctt', 'top:' + (vh() + 50) + 'px'); S.pctt.className = 'b';
      S.o_pct.observe(S.pctt);
      S.zero = div('zero', 'position:absolute;left:0;top:10px;width:200px;height:0');
      S.none = div('none', 'display:none;width:20px;height:20px');
      S.hid = div('hid', 'position:absolute;left:300px;top:0;width:20px;height:20px;visibility:hidden');
      S.det = document.createElement('div');
      S.o_misc = mk('misc'); for (const x of [S.zero, S.none, S.hid, S.det]) S.o_misc.observe(x);
      S.clip = div('clip', 'position:absolute;left:400px;top:0;width:100px;height:100px;overflow:hidden');
      S.clipped = div('clipped', 'position:absolute;left:0;top:150px;width:50px;height:50px', S.clip);
      S.o_clip = mk('clip'); S.o_clip.observe(S.clipped);
      S.sc = div('sc', 'position:absolute;left:600px;top:0;width:100px;height:100px;overflow:auto;border:5px solid;padding:3px');
      div('scfill', 'height:500px', S.sc);
      S.inner = div('inner', 'width:50px;height:50px', S.sc);
      S.o_sc = mk('sc', { root: S.sc }); S.o_sc.observe(S.inner);
      S.o_notdesc = mk('notdesc', { root: S.sc }); S.o_notdesc.observe(S.inv);
      S.o_doc = mk('doc', { root: document }); S.o_doc.observe(S.inv);
      await settle();
      dumpUnordered(S.calls);
      return out.join('\n');
    }
    if (phase === 1) {
      S.mark = S.calls.length;
      S.half.style.top = '0px';        // ratio 0.5 -> 1: crosses the 1 threshold
      S.none.style.display = 'block';  // gets a box
      S.sc.scrollTop = 500;            // inner scrolls into its root
      S.clipped.style.top = '10px';    // no longer clipped out
      return out.join('\n');
    }
    await settle();
    dumpUnordered(S.calls, S.mark);
    return out.join('\n');
  }

  if (section === 'ro') {
    if (phase === 0) {
      css('html,body{margin:0;padding:0}');
      S.calls = [];
      S.b = div('b', 'position:absolute;left:0;top:0;width:100px;height:50px;padding:10px;border:2px solid');
      S.n = div('n', 'display:none;width:20px;height:20px');
      S.z = div('z', 'position:absolute;left:0;top:200px;width:0;height:0');
      S.s = document.createElement('span'); S.s.id = 's'; S.s.textContent = 'inline text'; document.body.appendChild(S.s);
      S.det = document.createElement('div'); S.det.id = 'det';
      S.ro1 = new ResizeObserver(recorder('ro1', S.calls, roEntry));
      for (const x of [S.b, S.n, S.z, S.s, S.det]) S.ro1.observe(x);
      S.ro2 = new ResizeObserver(recorder('ro2', S.calls, roEntry));
      S.ro2.observe(S.b, { box: 'border-box' });
      S.ro3 = new ResizeObserver(recorder('ro3', S.calls, roEntry));
      S.ro3.observe(S.b, { box: 'device-pixel-content-box' });
      T('observe box bogus', () => new ResizeObserver(() => {}).observe(S.b, { box: 'bogus' }));
      T('observe(5)', () => new ResizeObserver(() => {}).observe(5));
      T('unobserve(5)', () => new ResizeObserver(() => {}).unobserve(5));
      log('sync calls', S.calls.length);
      await settle();
      dump(S.calls);
      return out.join('\n');
    }
    if (phase === 1) {
      await settle();
      log('no change, calls', S.calls.length);
      S.mark = S.calls.length;
      S.b.style.width = '150px';
      S.n.style.display = 'block';
      S.b.style.left = '40px';          // a move is not a resize
      return out.join('\n');
    }
    if (phase === 2) {
      await settle();
      dump(S.calls, S.mark);
      S.mark = S.calls.length;
      S.ro1.unobserve(S.b);
      S.ro2.disconnect();
      S.b.style.width = '160px';
      return out.join('\n');
    }
    await settle();
    dump(S.calls, S.mark);
    return out.join('\n');
  }

  // The loop limit: a callback that keeps resizing what it observes.
  if (section === 'ro-loop') {
    css('html,body{margin:0;padding:0}');
    const seq = [];
    window.addEventListener('error', (e) => seq.push('error:' + e.constructor.name + ':' + e.message + ':error=' + e.error
      + ':file=' + (e.filename === location.href ? 'document' : JSON.stringify(e.filename)) + ':' + e.lineno + ':' + e.colno + ':cancelable=' + e.cancelable));
    const g = div('g', 'width:10px;height:10px');
    const parent = div('parent', 'width:100px;height:10px');
    const child = div('child', 'width:10px;height:5px', parent);
    let grow = 3;
    const ro = new ResizeObserver((entries) => {
      seq.push('cb:' + entries.map((e) => name(e.target) + '=' + num(e.contentRect.width)).join(','));
      if (grow > 0) { grow--; g.style.width = (parseInt(g.style.width, 10) + 1) + 'px'; }
    });
    ro.observe(g);
    // A callback that resizes a DEEPER observed element is delivered in the same frame, no error.
    let once = true;
    const ro2 = new ResizeObserver((entries) => {
      seq.push('cb2:' + entries.map((e) => name(e.target) + '=' + num(e.contentRect.width)).join(','));
      if (once) { once = false; child.style.width = '20px'; }
    });
    ro2.observe(parent);
    ro2.observe(child);
    await tick(600);
    out.push(...seq);
    return out.join('\n');
  }

  // A same-origin iframe's document as the root of a target that is NOT inside it.
  //
  // Targets INSIDE the frame are not asked here: `korvus-page` lays out no nested document
  // (`getBoundingClientRect` of any element in a srcdoc frame is 0,0,0,0), so their answers would
  // measure that, not the observer. Chrome 150 answers `in is=true b=0,0,50,50 rb=0,0,285,200` for a
  // frame-document root and `rb=0,0,VW,VH` for the implicit root.
  if (section === 'io-frame') {
    css('html,body{margin:0;padding:0}');
    const f = document.createElement('iframe');
    f.setAttribute('style', 'border:0;width:300px;height:200px');
    const loaded = new Promise((r) => { f.onload = r; });
    f.srcdoc = '<!doctype html><style>html,body{margin:0}</style><div id="in" style="width:50px;height:50px"></div><div style="height:1000px"></div><div id="below" style="width:50px;height:50px"></div>';
    document.body.appendChild(f);
    await loaded;
    const d = f.contentDocument;
    log('contentDocument', !!d);
    if (!d) return out.join('\n');
    const calls = [];
    const parentTarget = div('pt', 'width:10px;height:10px');
    const foreign = new IntersectionObserver(recorder('foreign-root', calls, ioEntry), { root: d });
    foreign.observe(parentTarget);
    await settle();
    dumpUnordered(calls);
    return out.join('\n');
  }

  // Where the callbacks run relative to the other task sources.
  if (section === 'order') {
    css('html,body{margin:0;padding:0}');
    const seq = [];
    const t = div('ot', 'width:50px;height:50px');
    // Two intersection observers with IDENTICAL callbacks: Chrome does not call different observers
    // back in a stable order (see `dumpUnordered`), but it does drain one callback's microtasks before
    // the next observer's callback, and runs a timeout either one queued only after both.
    const ioCallback = () => {
      seq.push('io');
      queueMicrotask(() => seq.push('io-microtask'));
      setTimeout(() => seq.push('io-timeout0'), 0);
      requestAnimationFrame(() => seq.push('io-raf'));
    };
    const io = new IntersectionObserver(ioCallback);
    const ro = new ResizeObserver(() => { seq.push('ro'); queueMicrotask(() => seq.push('ro-microtask')); });
    const io2 = new IntersectionObserver(ioCallback);
    requestAnimationFrame(() => seq.push('rafA'));
    io2.observe(t);
    io.observe(t);
    ro.observe(t);
    requestAnimationFrame(() => seq.push('rafB'));
    setTimeout(() => seq.push('timeout0'), 0);
    queueMicrotask(() => seq.push('microtask'));
    seq.push('sync');
    await tick(600);
    log('order', seq.join(','));
    return out.join('\n');
  }

  return 'unknown section ' + section;
}
