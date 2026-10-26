(async function () {
  // The handles requestAnimationFrame, requestIdleCallback and setTimeout answer (ABPROG batch 4, a bot-detection service D6): Blink
  // numbers each document's animation-frame callbacks and its idle callbacks apart, each from 1, and its timers per
  // window. Each row is [what, the handles in order]; a frame's are asked after the page's.
  const out = [];
  const raf = [requestAnimationFrame(() => {}), requestAnimationFrame(() => {})];
  const ric = [requestIdleCallback(() => {}), requestIdleCallback(() => {})];
  raf.push(requestAnimationFrame(() => {}));
  out.push(['page rAF', raf.join(',')]);
  out.push(['page rIC', ric.join(',')]);
  // a cancelled handle of one kind leaves the other kind's callback with that number alone
  let ran = '';
  const a = requestAnimationFrame(() => { ran += 'raf'; });
  // its `timeout` runs it however busy the page is, so the row does not race the idle period
  const b = requestIdleCallback(() => { ran += 'ric'; }, { timeout: 100 });
  cancelIdleCallback(a);
  out.push(['same numbers', a + ',' + b]);
  const f = document.createElement('iframe');
  document.body.append(f);
  const w = f.contentWindow;
  out.push(['frame rAF', [w.requestAnimationFrame(() => {}), w.requestAnimationFrame(() => {})].join(',')]);
  out.push(['frame rIC', [w.requestIdleCallback(() => {}), w.requestIdleCallback(() => {})].join(',')]);
  out.push(['frame timers', [w.setTimeout(() => {}), w.setInterval(() => {}, 1000)].join(',')]);
  // the page's rAF after the frame's: the page's own count goes on
  out.push(['page rAF again', String(requestAnimationFrame(() => {}))]);
  await new Promise((r) => setTimeout(r, 500));
  out.push(['ran', ran]);
  f.remove();
  return out;
})()
