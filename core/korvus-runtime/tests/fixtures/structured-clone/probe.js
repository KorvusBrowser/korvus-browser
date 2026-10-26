(() => {
  // structuredClone of every kind of value V8's serializer and Blink's host objects keep or refuse, as stock Chrome 150
  // answers (an interactive-widget challenge 2026-10-10: korvus answered a cloned Boolean / Number / BigInt / String OBJECT with a fake
  // wrapper -- `typeof` "object" but `Object.prototype.toString` "[object Object]" and no value -- and a cloned Symbol
  // object without the DataCloneError; a bot-challenge frame clones a BigInt object). Every row: the row's
  // name -> what the clone reads as, or the error.
  const r = {};
  const t = (name, f) => { try { r[name] = f(); } catch (e) { r[name] = 'THREW ' + e.name + ': ' + e.message; } };
const c = structuredClone(Object(611n));
  t('bigintObj', () => [typeof c, Object.prototype.toString.call(c), c instanceof BigInt, Object.getPrototypeOf(c) === BigInt.prototype]);
  t('bigintStr', () => 'TNMKX5' + c);
  t('bigintValueOf', () => typeof c.valueOf());
  t('bigintPrim', () => { const p = structuredClone(611n); return [typeof p, String(p)]; });
  const n = structuredClone(Object(5)), s = structuredClone(Object('x')), b = structuredClone(Object(true));
  t('numberObj', () => [typeof n, n + 1, Object.prototype.toString.call(n)]);
  t('stringObj', () => [typeof s, s + '!', s.length]);
  t('boolObj', () => [typeof b, String(b), Object.prototype.toString.call(b)]);
  t('regexp', () => { const x = structuredClone(/gule/gi); return [String(x), x.flags, x.lastIndex]; });
  t('date', () => { const d = structuredClone(new Date(0)); return [d instanceof Date, d.getTime()]; });
  t('symbolObj', () => structuredClone(Object(Symbol('q'))));
  t('err', () => { const e = structuredClone(new RangeError('m')); return [e instanceof RangeError, e.name, e.message, Object.keys(e)]; });
  const P = o => o === null ? 'null' : o === Object.prototype ? 'Object.prototype' : o === Array.prototype ? 'Array.prototype' : (o && o.constructor && o.constructor.name) || String(o);
  class A { constructor() { this.x = 1; } get g() { return 2; } }
  t('classInstance', () => { const c = structuredClone(new A()); return [P(Object.getPrototypeOf(c)), JSON.stringify(c), c instanceof A]; });
  t('nullProto', () => { const o = Object.create(null); o.k = 1; const c = structuredClone(o); return [P(Object.getPrototypeOf(c)), c.k]; });
  t('arrayHoles', () => { const c = structuredClone([, 1, , 3]); return [c.length, 0 in c, 1 in c, 2 in c, Object.keys(c).join()]; });
  t('arrayExtraProp', () => { const a = [1, 2]; a.foo = 'bar'; const c = structuredClone(a); return [c.foo, Object.keys(c).join()]; });
  t('arraySparseBig', () => { const a = []; a[1000] = 1; const c = structuredClone(a); return [c.length, Object.keys(c).join()]; });
  t('weakMap', () => structuredClone(new WeakMap()));
  t('weakSet', () => structuredClone(new WeakSet()));
  t('weakRef', () => structuredClone(new WeakRef({})));
  t('promise', () => structuredClone(Promise.resolve(1)));
  t('proxy', () => structuredClone(new Proxy({}, {})));
  t('proxyArr', () => structuredClone(new Proxy([1], {})));
  t('generator', () => structuredClone((function* () {})()));
  t('arguments', () => { const c = (function () { return structuredClone(arguments); })(1, 2); return [P(Object.getPrototypeOf(c)), JSON.stringify(c), Array.isArray(c)]; });
  t('mapIterator', () => structuredClone(new Map().entries()));
  t('getterOrder', () => { const log = []; const o = { get b() { log.push('b'); return 1; }, a: 0, get c() { log.push('c'); return 2; } }; structuredClone(o); return log.join(); });
  t('symbolKey', () => { const c = structuredClone({ [Symbol('s')]: 1, x: 2 }); return [Object.getOwnPropertySymbols(c).length, c.x]; });
  t('nonEnum', () => { const o = {}; Object.defineProperty(o, 'h', { value: 1, enumerable: false }); o.v = 2; const c = structuredClone(o); return Object.keys(c).join() + '|' + ('h' in c); });
  t('errorCause', () => { const e = new TypeError('m', { cause: 5 }); const c = structuredClone(e); return [c.name, c.message, c.cause, Object.getOwnPropertyNames(c).join()]; });
  t('errorCustomName', () => { const e = new Error('m'); e.name = 'Foo'; const c = structuredClone(e); return [c.name, c.constructor.name]; });
  t('aggregate', () => { const c = structuredClone(new AggregateError([1], 'agg')); return [c.name, c.constructor.name, c.message, 'errors' in c]; });
  t('errorExtraProp', () => { const e = new Error('m'); e.extra = 1; const c = structuredClone(e); return [c.extra, Object.keys(c).join()]; });
  t('regexpLastIndex', () => { const x = /a/g; x.lastIndex = 3; x.foo = 1; const c = structuredClone(x); return [c.lastIndex, c.foo]; });
  t('dateProp', () => { const d = new Date(5); d.foo = 1; const c = structuredClone(d); return [c.getTime(), c.foo]; });
  t('mapProp', () => { const m = new Map([[1, 2]]); m.foo = 1; const c = structuredClone(m); return [c.get(1), c.foo]; });
  t('typedSub', () => { class U extends Uint8Array {} const c = structuredClone(new U([1, 2])); return [c.constructor.name, c.length]; });
  t('typedProp', () => { const u = new Uint8Array(2); u.foo = 1; const c = structuredClone(u); return [c.foo, c.length]; });
  t('sharedBuf', () => { const c = structuredClone([new ArrayBuffer(4)].map(b => [b, b])[0]); return c[0] === c[1]; });
  t('cycle', () => { const o = {}; o.self = o; const c = structuredClone(o); return c.self === c; });
  t('wrapperProps', () => { const n = Object(5); n.foo = 1; const c = structuredClone(n); return [c + 1, c.foo]; });
  t('stringWrapperIdx', () => { const c = structuredClone(Object('ab')); return [c[0], c.length, Object.keys(c).join()]; });
  t('negZero', () => { const c = structuredClone([-0, Object(-0)]); return [Object.is(c[0], -0), Object.is(c[1].valueOf(), -0)]; });
  t('resizable', () => { const b = new ArrayBuffer(2, { maxByteLength: 8 }); const c = structuredClone(b); return [c.resizable, c.maxByteLength, c.byteLength]; });
  t('dataView', () => { const c = structuredClone(new DataView(new ArrayBuffer(8), 2, 4)); return [c.byteOffset, c.byteLength, c.buffer.byteLength]; });
  t('bigint64', () => { const c = structuredClone(new BigInt64Array([1n])); return [c.constructor.name, String(c[0])]; });
    return r;
})()
