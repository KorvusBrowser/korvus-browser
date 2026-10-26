(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = ['removed frame'];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const t = async (name, f) => { try { R.push([name, await f()]); } catch (e) { R.push([name, e.name + ': ' + e.message]); } };
  const ts = (o) => Object.prototype.toString.call(o);
  const order = [];
  const p1 = navigator.getBattery();
  p1.then(() => order.push('battery'));
  Promise.resolve().then(() => order.push('micro'));
  setTimeout(() => order.push('timeout0'), 0);
  const p2 = navigator.getBattery();
  await t('same promise', () => [p1 === p2, ts(p1)]);
  const b = await p1;
  await new Promise((r) => setTimeout(r, 50));
  await t('order', () => order.join(','));
  await t('same promise after', () => navigator.getBattery() === p1);
  await t('same manager', async () => (await navigator.getBattery()) === b);
  await t('own', () => [Object.getOwnPropertyNames(b).join(','), Object.getOwnPropertySymbols(b).length, Object.keys(b).join(','), JSON.stringify(b)]);
  await t('proto chain', () => [Object.getPrototypeOf(b) === BatteryManager.prototype, Object.getPrototypeOf(BatteryManager.prototype) === EventTarget.prototype, BatteryManager.name, BatteryManager.length, ts(BatteryManager.prototype)]);
  await t('ctor', () => { try { new BatteryManager(); return 'no throw'; } catch (e) { return e.name + ': ' + e.message; } });
  await t('ctor call', () => { try { BatteryManager(); return 'no throw'; } catch (e) { return e.name + ': ' + e.message; } });
  await t('values', () => [b.charging, b.chargingTime, b.dischargingTime, b.level, b.onchargingchange, b.onlevelchange]);
  await t('handler', () => { const f = function () {}; b.onlevelchange = f; const r = [b.onlevelchange === f]; b.onlevelchange = 5; r.push(b.onlevelchange); b.onlevelchange = null; return r; });
  await t('for-in', () => { const a = []; for (const k in b) a.push(k); return a.join(','); });
  await t('descriptors', () => Object.getOwnPropertyNames(BatteryManager.prototype).map((k) => { const d = Object.getOwnPropertyDescriptor(BatteryManager.prototype, k); return k + ':' + (d.get ? 'g' : '') + (d.set ? 's' : '') + ('value' in d ? 'v' : '') + (d.enumerable ? 'e' : '') + (d.configurable ? 'c' : ''); }).join(' '));
  await t('getter bad receiver', () => { try { return Object.getOwnPropertyDescriptor(BatteryManager.prototype, 'level').get.call({}); } catch (e) { return e.name + ': ' + e.message; } });
  await t('getBattery bad receiver', async () => { try { const p = navigator.getBattery.call({}); return [ts(p), await p.then(() => 'resolved', (e) => e.name + ': ' + e.message)]; } catch (e) { return 'sync ' + e.name + ': ' + e.message; } });
  await t('getBattery props', () => { const d = Object.getOwnPropertyDescriptor(Navigator.prototype, 'getBattery'); return [typeof d.value, d.value.length, d.value.name, d.enumerable, d.writable, d.configurable, 'getBattery' in self.WorkerNavigator.prototype]; });
  await t('child frame', async () => { const f = document.createElement('iframe'); document.body.appendChild(f); const n = f.contentWindow.navigator; const cb = await n.getBattery(); return [cb === b, Object.getPrototypeOf(cb) === f.contentWindow.BatteryManager.prototype, n.getBattery() === n.getBattery()]; });
  await t('removed frame', async () => { const f = document.createElement('iframe'); document.body.appendChild(f); const n = f.contentWindow.navigator; f.remove(); try { const p = n.getBattery(); return [ts(p), await Promise.race([p.then((x) => 'resolved ' + ts(x), (e) => e.name + ': ' + e.message), new Promise((r) => setTimeout(() => r('pending'), 300))])]; } catch (e) { return 'sync ' + e.name + ': ' + e.message; } });
  await t('addEventListener', () => { let n = 0; const f = () => n++; b.addEventListener('levelchange', f); b.dispatchEvent(new Event('levelchange')); b.removeEventListener('levelchange', f); return n; });
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
