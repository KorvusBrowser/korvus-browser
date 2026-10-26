(async () => {
  const out = [];
  const o = indexedDB;
  const name = 'korvus-probe-' + Math.random().toString(36).slice(2);
  const req = o.open(name);
  out.push(['same-turn', JSON.stringify((await o.databases()).filter(d => d.name === name).length)]);
  await new Promise(r => { req.onsuccess = r; req.onerror = r; });
  out.push(['after-success', JSON.stringify((await o.databases()).filter(d => d.name === name).map(d => d.version))]);
  req.result.close();
  const del = o.deleteDatabase(name);
  await new Promise(r => { del.onsuccess = r; del.onerror = r; });
  out.push(['after-delete', JSON.stringify((await o.databases()).filter(d => d.name === name).length)]);
  const order = [];
  const r1 = o.open(name + 'b');
  r1.onupgradeneeded = () => order.push('upgrade');
  r1.onsuccess = () => order.push('success');
  Promise.resolve().then(() => order.push('microtask'));
  setTimeout(() => order.push('timeout0'), 0);
  await new Promise(r => setTimeout(r, 100));
  r1.result && r1.result.close();
  o.deleteDatabase(name + 'b');
  return out;
})()
