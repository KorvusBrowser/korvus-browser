// The worker global's own symbol keys (Object.getOwnPropertySymbols and Reflect.ownKeys), what is under
// Symbol.for('korvus.bridge') on it, and whether that name is anywhere on its chain.
function probe() {
  const B = Symbol.for('korvus.bridge');
  const syms = Object.getOwnPropertySymbols(self).map(String).join(',');
  const own = Reflect.ownKeys(self).filter((k) => typeof k === 'symbol').length;
  return ['syms=' + syms, 'ownSymbols=' + own, 'bridge=' + typeof self[B], 'inChain=' + (B in self)].join(' | ');
}
