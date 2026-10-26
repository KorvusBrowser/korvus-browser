(async () => {
  const rows = [];
  const st = document.createElement('style');
  st.textContent = `
    .box { container-type: inline-size; width: 200px; }
    .box.wide { width: 600px; }
    .t { opacity: 1; }
    @container (min-width: 400px) { .t { transition: opacity 80ms linear; opacity: 0.2; } }
    @container (min-width: 400px) { .a { animation: kf 80ms linear; } }
    @keyframes kf { from { color: rgb(255, 0, 0); } to { color: rgb(0, 0, 255); } }
    .q { width: 50cqw; transition: width 80ms linear; }
  `;
  document.head.appendChild(st);
  // A container already wide when it is inserted: the element's FIRST style has the rule, so nothing transitions.
  const box2 = document.createElement('div'); box2.className = 'box wide';
  const t2 = document.createElement('div'); t2.className = 't'; t2.textContent = 'z';
  box2.append(t2); document.body.append(box2);
  rows.push(['first style inside a wide container: opacity', getComputedStyle(t2).opacity]);
  rows.push(['first style inside a wide container: getAnimations', t2.getAnimations().length]);
  const box = document.createElement('div'); box.className = 'box';
  const t = document.createElement('div'); t.className = 't'; t.textContent = 'x';
  const a = document.createElement('div'); a.className = 'a'; a.textContent = 'y';
  const q = document.createElement('div'); q.className = 'q'; q.textContent = 'q';
  box.append(t, a, q); document.body.append(box);
  rows.push(['before the resize: opacity', getComputedStyle(t).opacity]);
  rows.push(['before the resize: animations on .a', a.getAnimations().length]);
  rows.push(['before the resize: cqw width', getComputedStyle(q).width]);
  box.classList.add('wide');
  rows.push(['after the resize: opacity (a transition starts at 1)', getComputedStyle(t).opacity]);
  rows.push(['after the resize: getAnimations on .t', t.getAnimations().map(x => x.constructor.name + ':' + x.transitionProperty).join()]);
  rows.push(['after the resize: getAnimations on .a', a.getAnimations().map(x => x.constructor.name + ':' + x.animationName).join()]);
  rows.push(['after the resize: cqw width (a transition starts at 100px)', getComputedStyle(q).width]);
  rows.push(['after the resize: getAnimations on .q', q.getAnimations().map(x => x.constructor.name + ':' + x.transitionProperty).join()]);
  await new Promise(r => setTimeout(r, 400));
  rows.push(['settled: opacity', getComputedStyle(t).opacity]);
  rows.push(['settled: cqw width', getComputedStyle(q).width]);
  rows.push(['settled: animations left', document.getAnimations().length]);
  return rows;
})()
