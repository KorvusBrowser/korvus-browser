(async () => {
  const out = [];
  const t = async (label, f) => { try { const r = f(); if (r && r.then) { await r.then(() => out.push([label, 'resolved']), (e) => out.push([label, 'rejected ' + e.name + ': ' + e.message])); } else out.push([label, 'ok']); } catch (e) { out.push([label, e.name + ': ' + e.message]); } };
  await t('geo cb 5', () => navigator.geolocation.getCurrentPosition(5));
  await t('geo err 5', () => navigator.geolocation.getCurrentPosition(() => {}, 5));
  await t('new ResizeObserver 5', () => new ResizeObserver(5));
  await t('rAF 5', () => requestAnimationFrame(5));
  await t('locks.request 5', () => navigator.locks.request('x', 5));
  await t('toBlob 5', () => document.createElement('canvas').toBlob(5));
  await t('treeWalker 5', () => document.createTreeWalker(document, 0, 5));
  await t('treeWalker obj', () => document.createTreeWalker(document, 0, {}));
  await t('rIC 5', () => requestIdleCallback(5));
  await t('queueMicrotask 5', () => queueMicrotask(5));
  await t('MutationObserver 5', () => new MutationObserver(5));
  await t('PerformanceObserver 5', () => new PerformanceObserver(5));
  await t('decodeAudioData 5', () => new OfflineAudioContext(1, 1, 44100).decodeAudioData(new ArrayBuffer(1), 5));
  await t('webkitGetAsEntry? setTimeout str', () => setTimeout('0'));
  await t('getCurrentPosition symbol options', () => navigator.geolocation.getCurrentPosition(() => {}, null, 5));
  await t('IdleDetector.start 5', () => new IdleDetector().start(5));
  await t('serial 5', () => navigator.serial.requestPort(5));
  await t('hid sym', () => navigator.hid.requestDevice({filters: Symbol()}));
  await t('AbortSignal.timeout -1', () => AbortSignal.timeout(-1));
  return out;
})()
