(async () => {
  const long = (p) => (p || performance).getEntries().filter((e) => e.entryType === 'long-animation-frame' && e.duration >= 70);
  const kind = (e) => (e.renderStart > 0 ? 'rendered' : 'task');
  const sleep = (ms) => new Promise((res) => setTimeout(res, ms));
  const busy = (ms) => { const t = performance.now(); while (performance.now() - t < ms); };
  const f = document.createElement('iframe');
  document.body.appendChild(f);
  await sleep(300);
  const cw = f.contentWindow;
  const cn = long(cw.performance).length;
  const t0 = performance.now();
  // the frame's request is answered in 100 ms and its load handler is busy 80 ms; the page's timer is due in 150 ms
  // and busy 80 ms too; the page is busy 300 ms meanwhile -- when it is done, the timer and the request's load are due
  // together, and run as two long tasks, each its own realm's, whichever runs first
  cw.eval("var x = new XMLHttpRequest(); x.open('GET', '/slow'); x.onload = function () { var t = performance.now(); while (performance.now() - t < 80); }; x.send();");
  setTimeout(function () { busy(80); }, 150);
  busy(300);
  await sleep(600);
  // the page's frames from this task on (its first frame's entry can come late, and is not this probe's)
  return [['frame-long-frames', long(cw.performance).slice(cn).map(kind)], ['page-long-frames', long().filter((e) => e.startTime >= t0 - 50).map(kind)]];
})()
