(async () => {
  const r = [];
  const run = (name, f) => { try { r.push([name, f()]); } catch (e) { r.push([name, 'THROW ' + e.name + ': ' + e.message]); } };
  // the pair iterables: `forEach` and the iterators walk the list as it stands at each step, by index
  const pairs = {
    URLSearchParams: { make: () => new URLSearchParams('a=1&b=2&c=3&d=4'), del: (m, k) => m.delete(k), add: (m, k) => m.append(k, '9') },
    Headers: { make: () => new Headers([['a', '1'], ['b', '2'], ['c', '3'], ['d', '4']]), del: (m, k) => m.delete(k), add: (m, k) => m.append(k, '9') },
    FormData: { make: () => { const f = new FormData(); for (const k of 'abcd') f.append(k, String('abcd'.indexOf(k) + 1)); return f; }, del: (m, k) => m.delete(k), add: (m, k) => m.append(k, '9') },
  };
  for (const [n, K] of Object.entries(pairs)) {
    run(n + ' thisArg', () => { const o = {tag: 'T'}; const seen = []; K.make().forEach(function (v, k, m) { seen.push(k + '=' + v + ':' + (this && this.tag) + ':' + (m instanceof self[n])); }, o); return seen.join(','); });
    run(n + ' non-callable', () => { K.make().forEach(5); return 'no throw'; });
    run(n + ' no callback', () => { K.make().forEach(); return 'no throw'; });
    run(n + ' throw stops', () => { const seen = []; try { K.make().forEach((v, k) => { seen.push(k); throw new Error('x'); }); } catch (e) { seen.push('caught ' + e.message); } return seen.join(','); });
    const walk = (name, at, act) => run(n + ' forEach ' + name, () => { const m = K.make(); const seen = []; m.forEach((v, k) => { seen.push(k); if (k === at) act(m); }); return seen.join(','); });
    walk('delete current b', 'b', (m) => K.del(m, 'b'));
    walk('delete earlier a at c', 'c', (m) => K.del(m, 'a'));
    walk('delete later d at b', 'b', (m) => K.del(m, 'd'));
    walk('add e at b', 'b', (m) => K.add(m, 'e'));
    walk('re-add a at b', 'b', (m) => { K.del(m, 'a'); K.add(m, 'a'); });
    walk('add 0 at b', 'b', (m) => K.add(m, '0'));
    const iter = (name, act) => run(n + ' iterator ' + name, () => {
      const m = K.make(); const it = m[Symbol.iterator](); const seen = [it.next().value[0], it.next().value[0]];
      act(m);
      for (let x = it.next(); !x.done; x = it.next()) seen.push(x.value[0]);
      return seen.join(',');
    });
    iter('delete current b', (m) => K.del(m, 'b'));
    iter('delete earlier a', (m) => K.del(m, 'a'));
    iter('delete later d', (m) => K.del(m, 'd'));
    iter('add e', (m) => K.add(m, 'e'));
    iter('re-add a', (m) => { K.del(m, 'a'); K.add(m, 'a'); });
    run(n + ' iterator exhausted then add', () => { const m = K.make(); const it = m.keys(); while (!it.next().done) {} K.add(m, 'z'); const x = it.next(); return x.done + ':' + x.value; });
  }
  // every maplike and setlike forEach: the argument count, then the callback's type, `thisArg`, a throw ending the walk
  const el = document.createElement('div'); document.body.appendChild(el);
  el.attributeStyleMap.set('width', CSS.px(5)); el.attributeStyleMap.set('height', CSS.px(6));
  const rng = document.createRange(); rng.selectNodeContents(document.body);
  CSS.highlights.set('h1', new Highlight()); CSS.highlights.set('h2', new Highlight());
  customElements.define('x-st', class extends HTMLElement { constructor() { super(); this.i = this.attachInternals(); } });
  const xs = document.createElement('x-st'); xs.i.states.add('s1'); xs.i.states.add('s2');
  let klm = null; try { klm = await navigator.keyboard.getLayoutMap(); } catch (e) {}
  const cases = [['KeyboardLayoutMap', klm], ['EventCounts', performance.eventCounts], ['StylePropertyMap', el.attributeStyleMap],
    ['Highlight', new Highlight(rng, document.createRange())], ['HighlightRegistry', CSS.highlights], ['CustomStateSet', xs.i.states],
    ['FontFaceSet', document.fonts], ['WGSLLanguageFeatures', navigator.gpu && navigator.gpu.wgslLanguageFeatures]];
  for (const [n, inst] of cases) {
    if (!inst) { r.push([n, 'no instance']); continue; }
    run(n + ' thisArg', () => { const o = {tag: 'T'}; let c = 0, t = []; inst.forEach(function (v, k, m) { c++; if (c < 3) t.push((this && this.tag) + ':' + (m === inst)); }, o); return c + ' ' + t.join(','); });
    run(n + ' non-callable', () => { inst.forEach(5); return 'no throw'; });
    run(n + ' no callback', () => { inst.forEach(); return 'no throw'; });
    run(n + ' throw stops', () => { let c = 0; try { inst.forEach(() => { c++; throw new Error('x'); }); } catch (e) { return c + ' caught ' + e.message; } return c + ' no throw'; });
  }
  run('StylePropertyMap delete during', () => { const seen = []; el.attributeStyleMap.forEach((v, k) => { seen.push(k); if (seen.length === 1) el.attributeStyleMap.delete('height'); }); return seen.join(','); });
  return r;
})()
