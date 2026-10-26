(async () => {
  const r = [];
  const desc = (a) => a === null ? 'null' : typeof a === 'object' ? Object.prototype.toString.call(a) + (a && a.constructor ? ':' + a.constructor.name : '') : typeof a + ':' + String(a).slice(0, 80);
  const run = (name, trigger) => new Promise((res) => {
    let done = false;
    window.onerror = function () { if (done) return; done = true; r.push([name, 'onerror', arguments.length, ...Array.from(arguments).map(desc), arguments[4] && String(arguments[4].stack).replace(/:\d{4,5}\//g, ':P/')]); window.onerror = null; res(); return true; };
    trigger();
    setTimeout(() => { if (!done) { done = true; r.push([name, 'no-onerror']); window.onerror = null; res(); } }, 300);
  });
  await run('string-timer-throw', () => setTimeout('\n  throw new Error("boom")', 0));
  await run('fn-timer-throw', () => setTimeout(function ft() {
    throw new TypeError('bad'); }, 0));
  await run('fn-timer-ref', () => setTimeout(function fr() { undefinedThing.x; }, 0));
  await run('listener-throw', () => { const d = document.createElement('div'); d.addEventListener('x', function lt() { throw new RangeError('rr'); }); d.dispatchEvent(new Event('x')); });
  await run('throw-string', () => setTimeout(function ts() { throw 'plain'; }, 0));
  await run('throw-object', () => setTimeout(function to() { throw {a: 1}; }, 0));
  await run('promise-reject', () => { Promise.reject(new Error('rej')); });
  // the error event too
  await new Promise((res) => {
    const h = (e) => { removeEventListener('error', h); r.push(['error-event', desc(e), e.message, String(e.filename).replace(/:\d{4,5}\//, ':P/'), e.lineno, e.colno, desc(e.error)]); e.preventDefault(); res(); };
    addEventListener('error', h);
    setTimeout(function ev() { null.x; }, 0);
  });
  r.push(['onerror-attr-type', typeof window.onerror]);
  // an onerror set as a content attribute on body
  document.body.setAttribute('onerror', 'window.__bodyArgs = Array.from(arguments).map(a => typeof a + ":" + (a && a.constructor ? a.constructor.name : String(a))); return true;');
  await new Promise((res) => { setTimeout(function bt() { throw new Error('via-body-attr'); }, 0); setTimeout(res, 50); });
  r.push(['body-attr-args', window.__bodyArgs]);
  document.body.removeAttribute('onerror');
  return r;
})()
