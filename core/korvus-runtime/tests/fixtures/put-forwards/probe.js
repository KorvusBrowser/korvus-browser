(() => {
  const R = [];
  const log = [];
  const hookGet = (P, k) => { const d = Object.getOwnPropertyDescriptor(P, k); if (!d || !d.get) return R.push('no getter ' + k); Object.defineProperty(P, k, { ...d, get() { log.push('get ' + k); return d.get.call(this); } }); };
  const hookSet = (P, k, name) => { const d = Object.getOwnPropertyDescriptor(P, k); if (!d || !d.set) return R.push('no setter ' + name); Object.defineProperty(P, k, { ...d, set(v) { log.push('set ' + name + '=' + v); return d.set.call(this, v); } }); };
  hookGet(HTMLIFrameElement.prototype, 'sandbox'); hookGet(Element.prototype, 'classList'); hookGet(HTMLElement.prototype, 'style'); hookGet(HTMLLinkElement.prototype, 'relList'); hookGet(HTMLAnchorElement.prototype, 'relList'); hookGet(HTMLOutputElement.prototype, 'htmlFor'); hookGet(Element.prototype, 'part'); hookGet(HTMLLinkElement.prototype, 'sizes'); hookGet(HTMLLinkElement.prototype, 'blocking');
  hookSet(DOMTokenList.prototype, 'value', 'DOMTokenList.value'); hookSet(CSSStyleDeclaration.prototype, 'cssText', 'CSSStyleDeclaration.cssText');
  const run = (label, f) => { log.length = 0; try { f(); } catch (e) { log.push('THROW ' + e.name + ': ' + e.message); } R.push(label + ' -> ' + log.join(', ')); };
  const ifr = document.createElement('iframe'); run('iframe.sandbox=', () => { ifr.sandbox = 'allow-scripts'; }); R.push(ifr.getAttribute('sandbox'));
  const d = document.createElement('div'); run('classList=', () => { d.classList = 'a b'; }); R.push(d.className);
  run('style=', () => { d.style = 'color: red'; }); R.push(d.getAttribute('style'));
  const l = document.createElement('link'); run('link.relList=', () => { l.relList = 'preload'; }); run('link.sizes=', () => { l.sizes = '16x16'; }); run('link.blocking=', () => { l.blocking = 'render'; });
  const a = document.createElement('a'); run('a.relList=', () => { a.relList = 'noopener'; });
  const o = document.createElement('output'); run('output.htmlFor=', () => { o.htmlFor = 'x y'; }); R.push(o.getAttribute('for'));
  run('part=', () => { d.part = 'p q'; }); R.push(d.getAttribute('part'));
  // a getter replaced by an own property: PutForwards uses Get(O, id), so an own data property wins
  const d2 = document.createElement('div'); const fake = { set value(v) { log.push('fake.value=' + v); } }; Object.defineProperty(d2, 'classList', { value: fake, configurable: true });
  run('own classList=', () => { d2.classList = 'z'; }); R.push(d2.className);
  run('classList=null', () => { d.classList = null; }); R.push(d.className);
  run('style setter receiver', () => { Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'style').set.call({}, 'x'); });
  run('sandbox setter on div', () => { Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'sandbox').set.call(d, 'x'); });
  const sv = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); document.body.appendChild(sv); log.length = 0; sv.style = 'fill: red'; R.push('svg style -> ' + log.join(', ') + ' attr=' + sv.getAttribute('style'));
  const sa = document.createElementNS('http://www.w3.org/2000/svg', 'a'); run('svg a.relList=', () => { sa.relList = 'x'; }); R.push(sa.getAttribute('rel'));
  const st = document.createElement('style'); st.textContent = '.q{color:blue}'; document.head.appendChild(st); const rule = st.sheet.cssRules[0]; run('rule.style=', () => { rule.style = 'color: green'; }); R.push(rule.cssText);
  const sc = document.createElement('script'); run('script.blocking=', () => { sc.blocking = 'render'; }); const vd = document.createElement('video'); run('controlsList=', () => { vd.controlsList = 'nodownload'; }); const fm = document.createElement('form'); run('form.relList=', () => { fm.relList = 'noreferrer'; }); const ar = document.createElement('area'); run('area.relList=', () => { ar.relList = 'noopener'; });
  return R;
})()
