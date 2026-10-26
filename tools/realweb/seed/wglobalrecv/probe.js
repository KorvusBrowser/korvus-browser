// TEMPLATES P6 B13: a worker scope's [Global] members check their receiver is the global (Illegal
// invocation for {} and Object.create(<Scope>.prototype)), count their arguments, and accept the global.
function probe(keys, methods, extra) {
  const out = [];
  const t = (label, f) => {
    try { const v = f(); out.push(label + '=' + (v === undefined ? 'undefined' : v === null ? 'null' : (v && typeof v.then === 'function') ? 'promise' : typeof v)); }
    catch (e) { out.push(label + '!' + e.constructor.name + ':' + e.message); }
  };
  const fake = Object.create(Object.getPrototypeOf(self));
  for (const k of keys) {
    const d = Object.getOwnPropertyDescriptor(self, k);
    if (!d) { out.push(k + ' absent'); continue; }
    t(k + '.get.obj', () => d.get.call({}));
    t(k + '.get.fake', () => d.get.call(fake));
    t(k + '.get.self', () => d.get.call(self));
    if (d.set) { t(k + '.set.obj', () => d.set.call({}, null)); t(k + '.set.fake', () => d.set.call(fake, null)); }
  }
  for (const [k, selfArgs] of methods) {
    const f = self[k];
    t(k + '.obj', () => f.call({}));
    t(k + '.fake', () => f.call(fake));
    if (selfArgs) for (const a of selfArgs) t(k + '.self' + a.length, () => f.apply(self, a));
  }
  if (extra) extra(t);
  return out.join(' | ');
}
