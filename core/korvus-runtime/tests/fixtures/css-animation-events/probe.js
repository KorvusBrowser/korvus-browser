(async () => {
  const rows = [];
  const st = document.createElement('style');
  st.textContent = `
    .one { transition: opacity 60ms linear; opacity: 1; } .one.on { opacity: 0.5; }
    .two { transition: opacity 60ms linear, color 60ms linear; opacity: 1; color: rgb(0, 0, 0); }
    .two.on { color: rgb(0, 0, 255); opacity: 0.5; }
    .late { transition: opacity 60ms linear 100ms; opacity: 1; } .late.on { opacity: 0.5; }
    .long { transition: color 400ms linear; color: rgb(0, 0, 0); } .long.on { color: rgb(255, 0, 0); } .long.on.again { color: rgb(0, 128, 0); }
    .gone { transition: opacity 400ms linear; opacity: 1; } .gone.on { opacity: 0.5; } .gone.hide { display: none; }
    .it { animation: kf 60ms linear 2; }
    .an { animation: kf 400ms linear; } .an.hide { display: none; }
    @keyframes kf { from { opacity: 0.25; } to { opacity: 0.75; } }
    .menu { display: none; opacity: 0; transition: opacity 60ms linear; } .menu.open { display: block; opacity: 1; }
    .shut { display: none; } .kid { opacity: 0; transition: opacity 60ms linear; } .kid.open { opacity: 1; }
  `;
  document.head.appendChild(st);
  const log = [];
  const t2 = e => (e.propertyName ? e.propertyName : e.animationName);
  const timed = new Set(['transitionrun', 'transitionstart', 'transitionend', 'animationstart', 'animationiteration', 'animationend']);
  for (const type of ['transitionrun', 'transitionstart', 'transitionend', 'transitioncancel', 'animationstart', 'animationiteration', 'animationend', 'animationcancel'])
    document.addEventListener(type, e => log.push([type, e.target.id, t2(e), timed.has(type) ? e.elapsedTime : '-', e.bubbles, e.cancelable, e.pseudoElement, e.isTrusted, e.constructor.name].join(':')));
  const el = (id, cls) => { const d = document.createElement('div'); d.id = id; d.className = cls; d.textContent = id; document.body.append(d); getComputedStyle(d).opacity; getComputedStyle(d).color; return d; };
  const wait = ms => new Promise(r => setTimeout(r, ms));
  const take = name => { rows.push([name, log.splice(0).join(' ')]); };

  const a = el('a', 'one'); a.classList.add('on'); getComputedStyle(a).opacity;
  await wait(250); take('one transition');

  const b = el('b', 'two'); const c = el('c', 'one'); b.classList.add('on'); c.classList.add('on'); getComputedStyle(b).opacity; getComputedStyle(c).opacity;
  await wait(250); take('three transitions starting in one frame: b opacity+color, c opacity');

  const d = el('d', 'late'); d.classList.add('on'); getComputedStyle(d).opacity;
  await wait(350); take('a transition with a 100ms delay');

  const f = el('f', 'long'); f.classList.add('on'); getComputedStyle(f).color;
  await wait(100); f.classList.add('again'); getComputedStyle(f).color;
  await wait(550); take('a transition interrupted by a new target');

  const g = el('g', 'gone'); g.classList.add('on'); getComputedStyle(g).opacity;
  await wait(100); g.classList.add('hide'); getComputedStyle(g).opacity;
  await wait(250); take('display:none during a transition');

  const h = el('h', 'it');
  await wait(300); take('an animation of two iterations');

  const k = el('k', 'an');
  await wait(100); k.classList.add('hide'); getComputedStyle(k).opacity;
  await wait(250); take('display:none during an animation');

  const n = el('n', 'menu'); n.classList.add('open'); getComputedStyle(n).opacity;
  await wait(250); take('display:none to block with an opacity change, the element itself');

  const p = el('p', 'shut'); const q = document.createElement('div'); q.id = 'q'; q.className = 'kid'; q.textContent = 'q'; p.append(q);
  getComputedStyle(q).opacity; p.classList.remove('shut'); q.classList.add('open'); getComputedStyle(q).opacity;
  await wait(250); take('display:none to block on the parent, the child changing opacity');

  const m = el('m', 'one'); m.onanimationstart = null; let order = [];
  m.addEventListener('transitionrun', () => order.push('listener'));
  m.ontransitionrun = () => order.push('handler');
  requestAnimationFrame(() => order.push('raf'));
  m.classList.add('on'); getComputedStyle(m).opacity;
  await wait(250); log.splice(0); rows.push(['transitionrun against rAF in one frame', order.join(' ')]);
  return rows;
})()
