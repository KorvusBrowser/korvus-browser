(async () => {
  // IndexedDB's connection queue and when the backend answers -- the ORDER of events against timers, never the
  // milliseconds -- recorded only after a warm-up, since the first database a profile creates is slower
  const R = [];
  const idb = indexedDB;
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const settle = (req) => new Promise((res) => { req.onsuccess = req.onerror = req.onblocked = () => res(); });
  const track = (ev, label, req) => {
    for (const t of ['upgradeneeded', 'success', 'error', 'blocked']) req.addEventListener(t, () => {
      ev.push(label + '.' + t);
      if (t === 'success' || t === 'upgradeneeded') {
        const db = req.result;
        if (db && db.addEventListener && !db.__t) { db.__t = 1; db.addEventListener('versionchange', (e) => { ev.push(label + '.conn.versionchange ' + e.oldVersion + '>' + e.newVersion); if (req.__closeOnVc) { db.close(); ev.push(label + '.conn.closed'); } }); }
      }
    });
    return req;
  };
  { const r = idb.open('warm'); await settle(r); r.result && r.result.close(); await settle(idb.deleteDatabase('warm')); }
  // 1. the connection queue: a request waits until every request before it on the same name is processed
  {
    const ev = [];
    const o1 = track(ev, 'o1', idb.open('q1'));
    track(ev, 'd', idb.deleteDatabase('q1'));
    const o2 = track(ev, 'o2', idb.open('q1'));
    await wait(400);
    R.push(['open, delete, open; nobody closes', ev.slice()]);
    o1.result && o1.result.close();
    await wait(300);
    R.push(['... then the first connection closes', ev.slice()]);
    o2.result && o2.result.close();
  }
  {
    const ev = [];
    const o1 = track(ev, 'o1', idb.open('q2'));
    o1.__closeOnVc = true;
    track(ev, 'd', idb.deleteDatabase('q2'));
    const o2 = track(ev, 'o2', idb.open('q2', 3));
    await wait(400);
    R.push(['open, delete, open v3; closed on versionchange', ev.slice()]);
    o2.result && o2.result.close();
  }
  {
    const ev = [];
    const o1 = track(ev, 'o1', idb.open('q3', 1));
    const o2 = track(ev, 'o2', idb.open('q3', 2));
    o1.__closeOnVc = true;
    await wait(400);
    R.push(['open v1, open v2 (closes on versionchange)', ev.slice()]);
    o2.result && o2.result.close();
  }
  // 2. when the backend answers. Its answers arrive after a 0 ms timer the page queues right after the call
  // (another process answers them); where they land against longer timers moves from run to run in Chrome
  // (`databases()` 0.5-5 ms, a new database 3-9 ms), so only the order of the events among themselves is kept.
  {
    const order = [];
    idb.databases().then(() => order.push('databases'));
    setTimeout(() => order.push('timeout 0'), 0);
    Promise.resolve().then(() => order.push('microtask'));
    await wait(100);
    R.push(['databases() against a microtask and a 0 ms timer', order]);
  }
  const race = async (label, start) => {
    const order = [];
    await start((what) => order.push(what));
    await wait(120);
    R.push([label, order]);
  };
  for (let round = 0; round < 2; round++) {
    await race('open a new database, then databases()', (mark) => new Promise((res) => {
      const r = idb.open('n' + round);
      r.onupgradeneeded = () => mark('upgradeneeded');
      r.onsuccess = () => { mark('success'); r.result.close(); res(); };
      idb.databases().then(() => mark('databases'));
    }));
    await race('delete it, then databases()', (mark) => new Promise((res) => {
      const d = idb.deleteDatabase('n' + round);
      d.onsuccess = () => { mark('success'); res(); };
      idb.databases().then(() => mark('databases'));
    }));
  }
  {
    const r = idb.open('w');
    r.onupgradeneeded = () => r.result.createObjectStore('s');
    await settle(r);
    const db = r.result;
    await race('a readwrite transaction', (mark) => new Promise((res) => {
      const tx = db.transaction('s', 'readwrite');
      tx.objectStore('s').put(1, 'k').onsuccess = () => mark('put');
      tx.oncomplete = () => { mark('complete'); res(); };
    }));
    db.close();
    await settle(idb.deleteDatabase('w'));
  }
  {
    const order = [];
    await new Promise((res) => {
      const r = idb.open('u');
      r.onupgradeneeded = () => { order.push('upgradeneeded'); r.transaction.oncomplete = () => order.push('complete'); };
      r.onsuccess = () => { order.push('success'); r.result.close(); res(); };
    });
    R.push(['an upgrade commits, then the open succeeds', order]);
    await settle(idb.deleteDatabase('u'));
  }
  // 3. what databases() lists: the backend's COMMITTED state when it answers
  const has = async (n) => (await idb.databases()).some((d) => d.name === n);
  {
    const r = idb.open('z');
    const opened = new Promise((res) => r.onsuccess = res);
    const atOpen = has('z');
    await opened;
    const afterOpen = await has('z');
    r.result.close();
    await wait(30);
    const d = idb.deleteDatabase('z');
    const order = [];
    d.onsuccess = () => order.push('success');
    const atDelete = await has('z').then((v) => { order.push('databases'); return v; });
    await wait(30);
    R.push(['open, then close, 30 ms, delete', [await atOpen, afterOpen, atDelete, order]]);
  }
  {
    const r1 = idb.open('y1', 1);
    await settle(r1);
    r1.result.close();
    await wait(30);
    const r2 = idb.open('y1', 2);
    const seen = [];
    r2.onupgradeneeded = async () => { seen.push(JSON.stringify((await idb.databases()).filter((d) => d.name === 'y1'))); };
    await new Promise((res) => r2.onsuccess = res);
    seen.push(JSON.stringify((await idb.databases()).filter((d) => d.name === 'y1')));
    r2.result.close();
    R.push(['an upgrade of an existing database', seen]);
    await settle(idb.deleteDatabase('y1'));
  }
  {
    const r = idb.open('y2');
    const seen = [];
    r.onupgradeneeded = async () => { seen.push(JSON.stringify((await idb.databases()).filter((d) => d.name === 'y2'))); };
    await new Promise((res) => r.onsuccess = res);
    r.result.close();
    R.push(['a first upgrade', seen]);
    await settle(idb.deleteDatabase('y2'));
  }
  // 4. a frame goes, its connections with it: the delete they blocked is carried out, silently
  {
    const mk = () => new Promise((res) => { const f = document.createElement('iframe'); f.src = '/frame?' + Math.random(); f.onload = () => res(f); document.body.appendChild(f); });
    const fa = await mk();
    const wa = fa.contentWindow;
    const ev = [];
    const o = wa.indexedDB.open('x');
    o.onsuccess = () => ev.push('open success'); o.onupgradeneeded = () => ev.push('open upgradeneeded');
    const d = wa.indexedDB.deleteDatabase('x');
    d.onsuccess = () => ev.push('delete success'); d.onblocked = () => ev.push('delete blocked');
    await wait(300);
    R.push(['in the frame', ev.slice(), await has('x')]);
    const order = [];
    fa.remove();
    const asked = [has('x').then((v) => order.push('same task ' + v))];
    for (const ms of [0, 2, 8]) asked.push(new Promise((res) => setTimeout(() => has('x').then((v) => { order.push('t' + ms + ' ' + v); res(); }), ms)));
    await Promise.all(asked);
    const fb = await mk();
    const inB = JSON.stringify((await fb.contentWindow.indexedDB.databases()).map((x) => x.name).filter((n) => n === 'x'));
    await wait(100);
    R.push(['the frame removed', order, inB, ev.slice()]);
    fb.remove();
  }
  for (const n of ['q1', 'q2', 'q3', 'n0', 'n1']) await settle(idb.deleteDatabase(n));
  return R;
})()
