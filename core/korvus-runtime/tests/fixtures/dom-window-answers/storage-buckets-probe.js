(function () {
  // navigator.storageBuckets as Chrome 150 answers it (modules/buckets, content/browser/buckets). Left out: a
  // deleted bucket's object (Chrome 150's tab crashed on it) and the usage an IndexedDB database adds (not modelled).
  var rows = [];
  function t(p, ms) { return Promise.race([p, new Promise(function (r) { setTimeout(function () { r('TIMEOUT'); }, ms || 3000); })]); }
  function settle(label, mk) {
    var p;
    try { p = mk(); } catch (e) { rows.push([label, 'sync throw', e.name, e.message]); return Promise.resolve(); }
    return t(Promise.resolve(p).then(function (v) { return ['ok', v]; }, function (e) { return ['reject', e && e.name, e && e.message]; })).then(function (r) {
      if (r === 'TIMEOUT') { rows.push([label, 'TIMEOUT']); return; }
      var v = r[1];
      if (r[0] === 'ok') rows.push([label, 'ok', v === undefined ? 'undefined' : (v === null ? 'null' : (typeof v === 'object' ? Object.prototype.toString.call(v) + ' ' + JSON.stringify(v) : typeof v + ' ' + String(v)))]);
      else rows.push([label].concat(r));
      return r;
    });
  }
  var sb = navigator.storageBuckets;
  rows.push(['typeof', typeof sb, Object.prototype.toString.call(sb), navigator.storageBuckets === sb]);
  rows.push(['manager proto names', Object.getOwnPropertyNames(StorageBucketManager.prototype).join(',')]);
  rows.push(['bucket proto names', Object.getOwnPropertyNames(StorageBucket.prototype).join(',')]);
  var b, b2;
  var chain = Promise.resolve();
  function then(f) { chain = chain.then(f); }
  then(function () { return settle('keys before', function () { return sb.keys(); }); });
  then(function () { return sb.open('fxbucket').then(function (x) { b = x; rows.push(['open', Object.prototype.toString.call(b), b.name, Object.keys(b).length]); }, function (e) { rows.push(['open', 'reject', e.name, e.message]); }); });
  then(function () { return sb.open('fxbucket').then(function (x) { b2 = x; rows.push(['open again', b2 === b, b2.indexedDB === b.indexedDB]); }); });
  ['A', '', '_a', '-a', 'a_b-c', 'a'.repeat(63), 'a'.repeat(64), 'a b', 'é', 'default'].forEach(function (n) {
    then(function () { return settle('open ' + JSON.stringify(n), function () { return sb.open(n).then(function (x) { return x.name; }); }); });
  });
  then(function () { return settle('open number', function () { return sb.open(1).then(function (x) { return x.name; }); }); });
  then(function () { return settle('open no args', function () { return sb.open(); }); });
  then(function () { return settle('open quota 0', function () { return sb.open('q0', { quota: 0 }); }); });
  then(function () { return settle('open quota -1', function () { return sb.open('q1', { quota: -1 }); }); });
  then(function () { return settle('open quota 1000', function () { return sb.open('q2', { quota: 1000 }).then(function (x) { return x.estimate(); }).then(function (e) { return e.quota + '/' + e.usage; }); }); });
  then(function () { return settle('open options 5', function () { return sb.open('q3', 5); }); });
  then(function () { return settle('open durability', function () { return sb.open('q4', { durability: 'strict' }).then(function (x) { return x.name; }); }); });
  then(function () { return settle('open persisted true', function () { return sb.open('q5', { persisted: true }).then(function (x) { return x.persisted(); }); }); });
  then(function () { return settle('open expires', function () { return sb.open('q6', { expires: 4102444800000 }).then(function (x) { return x.expires(); }); }); });
  then(function () { return settle('keys', function () { return sb.keys(); }); });
  then(function () { return settle('delete A', function () { return sb.delete('A'); }); });
  then(function () { return settle('delete missing', function () { return sb.delete('nothere'); }); });
  then(function () { return settle('delete no args', function () { return sb.delete(); }); });
  then(function () { return settle('name', function () { return b.name; }); });
  then(function () { return settle('persisted', function () { return b.persisted(); }); });
  then(function () { return settle('persist', function () { return b.persist(); }); });
  then(function () { return settle('persisted after persist', function () { return b.persisted(); }); });
  then(function () { return settle('expires', function () { return b.expires(); }); });
  then(function () { return settle('setExpires', function () { return b.setExpires(4102444800000); }); });
  then(function () { return settle('expires after set', function () { return b.expires(); }); });
  then(function () { return settle('setExpires past', function () { return b.setExpires(1000); }); });
  then(function () { return settle('setExpires no args', function () { return b.setExpires(); }); });
  then(function () {
    return t(Promise.all([b.estimate(), navigator.storage.estimate()])).then(function (r) {
      if (r === 'TIMEOUT') { rows.push(['estimate', 'TIMEOUT']); return; }
      rows.push(['estimate keys', Object.keys(r[0]).join(','), JSON.stringify(r[0].usageDetails), r[0].usage, r[0].quota === r[1].quota, typeof r[0].quota, r[0].quota > 0]);
    });
  });
  then(function () {
    rows.push(['indexedDB', Object.prototype.toString.call(b.indexedDB), b.indexedDB === b.indexedDB, b.indexedDB === indexedDB]);
    rows.push(['caches', Object.prototype.toString.call(b.caches), b.caches === b.caches, b.caches === caches]);
    rows.push(['locks', typeof b.locks, typeof b.durability]);
  });
  then(function () { return settle('bucket databases before', function () { return b.indexedDB.databases(); }); });
  then(function () {
    return t(new Promise(function (res, rej) {
      var r = b.indexedDB.open('1');
      r.onupgradeneeded = function () { r.result.createObjectStore('s'); };
      r.onsuccess = function () { var db = r.result; var tx = db.transaction('s', 'readwrite'); tx.objectStore('s').put('v', 'k'); tx.oncomplete = function () { db.close(); res('stored'); }; };
      r.onerror = function () { rej(r.error); };
    })).then(function (v) { rows.push(['bucket idb', v]); }, function (e) { rows.push(['bucket idb', 'error', e && e.name]); });
  });
  then(function () { return settle('bucket databases after', function () { return b.indexedDB.databases(); }); });
  then(function () { return settle('window databases after', function () { return indexedDB.databases().then(function (l) { return l.map(function (d) { return d.name; }); }); }); });
  then(function () { return settle('caches keys', function () { return b.caches.keys(); }); });
  then(function () { return settle('caches open', function () { return b.caches.open('c').then(function (c) { return Object.prototype.toString.call(c); }); }); });
  then(function () { return settle('bucket caches keys after', function () { return b.caches.keys(); }); });
  then(function () { return settle('window caches keys', function () { return caches.keys(); }); });
  then(function () { return settle('getDirectory', function () { return b.getDirectory().then(function (d) { return d.kind + '/' + JSON.stringify(d.name); }); }); });
  then(function () { return settle('getDirectory isSameEntry root', function () { return Promise.all([b.getDirectory(), navigator.storage.getDirectory()]).then(function (p) { return p[0].isSameEntry(p[1]); }); }); });
  then(function () { return settle('keys before delete', function () { return sb.keys(); }); });
  then(function () { return settle('delete fxbucket', function () { return sb.delete('fxbucket'); }); });
  then(function () { return settle('keys after delete', function () { return sb.keys(); }); });
  then(function () { try { StorageBucket.prototype.persisted.call({}); rows.push(['foreign persisted', 'no throw']); } catch (e) { rows.push(['foreign persisted', e.name, e.message]); } });
  then(function () { return settle('foreign persisted promise', function () { return StorageBucket.prototype.persisted.call({}); }); });
  then(function () { try { var n = Object.getOwnPropertyDescriptor(StorageBucket.prototype, 'name').get.call({}); rows.push(['foreign name', n]); } catch (e) { rows.push(['foreign name', e.name, e.message]); } });
  then(function () { return settle('foreign keys', function () { return StorageBucketManager.prototype.keys.call({}); }); });
  then(function () { try { new StorageBucket(); } catch (e) { rows.push(['new StorageBucket', e.name, e.message]); } });
  return chain.then(function () { return rows; }, function (e) { rows.push(['CHAIN REJECTED', String(e)]); return rows; });
})()
