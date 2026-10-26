(async () => {
  const R = [];
  const kb = navigator.keyboard;
  R.push(Object.prototype.toString.call(kb), kb === navigator.keyboard, Object.getOwnPropertyNames(Keyboard.prototype).join());
  const p1 = kb.getLayoutMap(), p2 = kb.getLayoutMap();
  R.push(p1 === p2);
  const m = await p1;
  R.push(Object.prototype.toString.call(m), m.size, m.get('KeyA'), m.get('Nope'), m.has('Digit1'), m.has(1));
  R.push([...m.keys()].slice(0, 6).join(), [...m.values()].slice(0, 6).join(), JSON.stringify([...m].slice(0, 2)));
  R.push(m[Symbol.iterator] === m.entries, Object.prototype.toString.call(m.entries()));
  let fe = []; m.forEach(function (v, k, mm) { if (fe.length < 2) fe.push(v + k + (mm === m) + (this === R)); }, R); R.push(fe.join());
  R.push((await kb.getLayoutMap()) === m);
  const err = async (p) => { try { return 'ok ' + String(await p); } catch (e) { return e.name + ': ' + e.message; } };
  R.push(await err(kb.lock()), await err(kb.lock(['KeyA', 'Digit1'])), await err(kb.lock(['KeyA', 'bogus'])), await err(kb.lock('xy')), await err(kb.lock(5)));
  const a = kb.lock(), b = kb.lock(); R.push(await err(a), await err(b));
  R.push(kb.unlock());
  R.push(await err(Keyboard.prototype.getLayoutMap.call({})));
  try { Keyboard.prototype.unlock.call({}); R.push('no throw'); } catch (e) { R.push(e.name + ': ' + e.message); }
  try { KeyboardLayoutMap.prototype.get.call({}, 'a'); } catch (e) { R.push(e.name + ': ' + e.message); }
  try { m.get(); } catch (e) { R.push(e.name + ': ' + e.message); }
  const f = document.createElement('iframe'); document.body.appendChild(f);
  const fk = f.contentWindow.navigator.keyboard;
  R.push(await err(fk.getLayoutMap().then((x) => x.size)), await err(fk.lock()));
  const q = navigator.webkitTemporaryStorage;
  R.push(Object.getOwnPropertyNames(Object.getPrototypeOf(q)).join(), q.constructor === Object, Object.prototype.toString.call(q));
  R.push(Object.prototype.toString.call(navigator.protectedAudience), Object.prototype.toString.call(navigator.login), navigator.deprecatedRunAdAuctionEnforcesKAnonymity);
  return R;
})()
