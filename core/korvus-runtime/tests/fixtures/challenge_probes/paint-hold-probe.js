(async () => {
  // Paint timing and the first long animation frame on a page whose body may draw nothing: which entries the
  // timeline holds as the page gains a box that draws, then text.
  const R = [];
  const types = () => performance.getEntries().map((e) => e.entryType + (e.entryType === 'paint' ? ':' + e.name : '')).filter((t) => t !== 'resource').join(',');
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  R.push(['at the script', types()]);
  await sleep(400);
  R.push(['400 ms later, nothing added', types()]);
  const nav = performance.getEntriesByType('navigation')[0];
  const l = performance.getEntriesByType('long-animation-frame')[0];
  R.push(['first long animation frame', l ? JSON.stringify({ beforeResponse: l.startTime < nav.responseStart, nearFetchStart: Math.abs(l.startTime - nav.fetchStart) < 60, rendersAfterResponse: l.renderStart >= nav.responseStart, long: l.duration >= 50 }) : 'none']);
  const box = document.createElement('div');
  box.style.cssText = 'width:50px;height:50px;border:2px solid red';
  document.body.append(box);
  await sleep(300);
  R.push(['a bordered empty box', types()]);
  const p = document.createElement('p');
  p.textContent = 'Verifying you are human.';
  document.body.append(p);
  await sleep(300);
  R.push(['then text', types()]);
  return R;
})()
