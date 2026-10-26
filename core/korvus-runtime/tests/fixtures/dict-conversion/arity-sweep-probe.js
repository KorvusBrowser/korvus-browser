(() => {
  // every interface object with a required-argument count N >= 2, constructed with N-1 `undefined` arguments: the
  // argument-count check must come before any conversion
  const out = [];
  const names = Object.getOwnPropertyNames(window).filter((n) => /^[A-Z]/.test(n)).sort();
  for (const n of names) {
    let f;
    try { f = window[n]; } catch (e) { continue; }
    if (typeof f !== 'function' || !(f.length >= 2)) continue;
    const args = new Array(f.length - 1).fill(undefined);
    let r;
    try { const v = Reflect.construct(f, args); r = 'ok ' + Object.prototype.toString.call(v); } catch (e) { r = e.name + ': ' + e.message; }
    out.push([n, f.length, r]);
  }
  return out;
})()
