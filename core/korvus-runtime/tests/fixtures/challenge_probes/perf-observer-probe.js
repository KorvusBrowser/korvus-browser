(async () => {
  // PerformanceObserver as Chrome 150 runs it (an interactive-widget challenge 2026-10-09): the callback's this / observer / options,
  // observe()'s dictionary read once in order, the observer-type lock, takeRecords
  const R = [];
  {
  
    const seen = [];
    const po = new PerformanceObserver(function (list, obs, opts) {
      seen.push(['this is observer', this === po, 'arguments', arguments.length, 'obs is observer', obs === po,
        'opts', opts === undefined ? 'undefined' : JSON.stringify(opts), 'opts proto', opts === undefined ? '-' : Object.getPrototypeOf(opts) === Object.prototype,
        'list class', Object.prototype.toString.call(list), 'entries', list.getEntries().length]);
    });
    po.observe({ type: 'mark' });
    performance.mark('a'); performance.mark('b');
    await new Promise((r) => setTimeout(r, 100));
    R.push(['type mark', seen.splice(0)]);
    const po2 = new PerformanceObserver(function (list, obs, opts) {
      seen.push(['buffered', arguments.length, opts === undefined ? 'undefined' : JSON.stringify(opts)]);
    });
    po2.observe({ type: 'mark', buffered: true });
    await new Promise((r) => setTimeout(r, 100));
    R.push(['buffered', seen.splice(0)]);
    const po3 = new PerformanceObserver(function (list, obs, opts) {
      seen.push(['entryTypes', arguments.length, opts === undefined ? 'undefined' : JSON.stringify(opts)]);
    });
    po3.observe({ entryTypes: ['mark'] });
    performance.mark('c');
    await new Promise((r) => setTimeout(r, 100));
    R.push(['entryTypes', seen.splice(0)]);
    po.disconnect(); po2.disconnect(); po3.disconnect();
    
  }
  {
  
    const spy = (o) => { const log = []; const p = new Proxy(o, { get(t, k, r) { log.push(String(k)); return Reflect.get(t, k, r); }, has(t, k) { log.push('has ' + String(k)); return Reflect.has(t, k); } }); return [p, log]; };
    const po = new PerformanceObserver(() => {});
    for (const init of [{ type: 'mark' }, { type: 'mark', buffered: true }, { entryTypes: ['mark', 'measure'] }, { type: 'mark', durationThreshold: 16 }]) {
      const [p, log] = spy(init);
      let r = 'ok';
      try { po.observe(p); } catch (e) { r = e.name + ': ' + e.message; }
      R.push([JSON.stringify(init), log.join(','), r]);
      po.disconnect();
    }
    const errs = [];
    for (const init of [{}, { type: 'mark', entryTypes: ['mark'] }, { entryTypes: 'mark' }, { entryTypes: 5 }, { type: 'nosuch' }, { entryTypes: [] }]) {
      let r = 'ok';
      try { po.observe(init); } catch (e) { r = e.name + ': ' + e.message; }
      errs.push([JSON.stringify(init), r]);
      po.disconnect();
    }
    R.push(['errors', errs]);
    let r2 = 'ok'; try { po.observe(); } catch (e) { r2 = e.name + ': ' + e.message; } R.push(['no arg', r2]);
    
  }
  {
  
    const tryObs = (po, init) => { try { po.observe(init); return 'ok'; } catch (e) { return e.name + ': ' + e.message; } };
    const a = new PerformanceObserver(() => {});
    R.push(['entryTypes then type', tryObs(a, { entryTypes: ['mark'] }), tryObs(a, { type: 'mark' })]);
    const b = new PerformanceObserver(() => {});
    R.push(['fresh empty entryTypes', tryObs(b, { entryTypes: [] })]);
    R.push(['then type after empty', tryObs(b, { type: 'mark' })]);
    const c = new PerformanceObserver(() => {});
    R.push(['entryTypes unsupported only', tryObs(c, { entryTypes: ['nosuch'] })]);
    const d = new PerformanceObserver(() => {});
    R.push(['entryTypes iterable Set', tryObs(d, { entryTypes: new Set(['mark']) })]);
    const seen = [];
    const e = new PerformanceObserver((l, o, opts) => seen.push(JSON.stringify(opts)));
    e.observe({ type: 'mark' }); performance.mark('x1');
    await new Promise((r) => setTimeout(r, 50));
    e.observe({ type: 'measure' }); performance.mark('x2');
    await new Promise((r) => setTimeout(r, 50));
    performance.mark('x3');
    await new Promise((r) => setTimeout(r, 50));
    e.disconnect(); e.observe({ type: 'mark' }); performance.mark('x4');
    await new Promise((r) => setTimeout(r, 50));
    e.disconnect();
    R.push(['options across observe calls', seen]);
    const tr = new PerformanceObserver(() => {}); tr.observe({ type: 'mark' }); performance.mark('y');
    const took = tr.takeRecords(); tr.disconnect();
    R.push(['takeRecords', took.length, Array.isArray(took)]);
    
  }
  return R;
})()
