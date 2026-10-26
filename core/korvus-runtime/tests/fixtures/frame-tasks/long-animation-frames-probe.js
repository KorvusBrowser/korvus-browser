(async () => {
  const r = [];
  const nav = performance.getEntriesByType('navigation')[0];
  const long = (p) => (p || performance).getEntries().filter((e) => e.entryType === 'long-animation-frame' && e.duration >= 70);
  const kind = (e) => (e.renderStart > 0 ? 'rendered' : 'task');
  const sleep = (ms) => new Promise((res) => setTimeout(res, ms));
  const busy = (ms) => { const t = performance.now(); while (performance.now() - t < ms); };
  await sleep(400);
  r.push(['frames-spanning-the-fetch', performance.getEntries().filter((e) => e.entryType === 'long-animation-frame' && e.startTime < nav.responseStart).length]);
  let n = long().length;
  await new Promise((res) => setTimeout(() => { busy(80); res(); }, 20));
  await sleep(300);
  r.push(['own-80ms-task', long().slice(n).map(kind)]);
  n = long().length;
  await new Promise((res) => setTimeout(() => { busy(80); document.body.appendChild(document.createElement('div')).textContent = 'x'; res(); }, 20));
  await sleep(400);
  r.push(['own-80ms-task-that-renders', long().slice(n).map(kind)]);
  n = long().length;
  await new Promise((res) => {
    addEventListener('message', function on(e) { if (e.data === 'busy') { removeEventListener('message', on); busy(80); res(); } });
    parent.postMessage('ask-busy', '*');
  });
  await sleep(300);
  r.push(['80ms-message-from-the-page', long().slice(n).map(kind)]);
  n = long().length;
  await new Promise((res) => {
    const x = new XMLHttpRequest();
    x.open('GET', '/f?x');
    x.onload = () => { busy(80); res(); };
    x.send();
  });
  await sleep(300);
  r.push(['80ms-xhr-load-handler', long().slice(n).map(kind)]);
  const f = document.createElement('iframe');
  document.body.appendChild(f);
  await sleep(300);
  const cw = f.contentWindow;
  let cn = long(cw.performance).length;
  n = long().length;
  cw.eval("setTimeout(function () { var t = performance.now(); while (performance.now() - t < 80); }, 20)");
  await sleep(400);
  r.push(['same-origin-childs-80ms-task', long(cw.performance).slice(cn).map(kind), long().slice(n).map(kind)]);
  cn = long(cw.performance).length;
  n = long().length;
  cw.eval("setTimeout(function () { var t = performance.now(); while (performance.now() - t < 80); document.body.appendChild(document.createElement('div')).textContent = 'y'; }, 20)");
  await sleep(400);
  r.push(['same-origin-childs-80ms-task-that-renders', long(cw.performance).slice(cn).map(kind), long().slice(n).map(kind)]);
  return r;
})()
