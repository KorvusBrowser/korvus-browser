(async () => {
  const out = [];
  const mk = async () => { const f = document.createElement('iframe'); document.body.appendChild(f); await new Promise(r => setTimeout(r, 50)); return f; };
  const f1 = await mk();
  out.push(['s1', String(f1.contentWindow.status)]);
  f1.contentWindow.status = 'x';
  out.push(['s1b', String(f1.contentWindow.status), String(window.status)]);
  out.push(['db1', JSON.stringify(await f1.contentWindow.indexedDB.databases())]);
  const req = f1.contentWindow.indexedDB.open('probe-db');
  await new Promise(r => { req.onsuccess = r; req.onerror = r; setTimeout(r, 1000); });
  out.push(['db1b', JSON.stringify(await f1.contentWindow.indexedDB.databases())]);
  f1.remove();
  const f2 = await mk();
  out.push(['s2', String(f2.contentWindow.status), String(window.status)]);
  out.push(['db2', JSON.stringify(await f2.contentWindow.indexedDB.databases())]);
  f2.remove();
  out.push(['dbtop', JSON.stringify(await indexedDB.databases())]);
  return out;
})()
