(async () => {
  const rows = [];
  const st = document.createElement('style');
  st.textContent = `
    .host { color: rgb(0, 0, 0); }
    .lite { transition: color 80ms linear; }
    @keyframes kf { from { opacity: 0.25; } to { opacity: 0.75; } }
    .spin { animation: kf 80ms linear; }
  `;
  document.head.appendChild(st);
  const wait = ms => new Promise(r => setTimeout(r, ms));
  const log = [];
  for (const type of ['transitionrun', 'transitionend', 'transitioncancel', 'animationstart', 'animationend'])
    document.addEventListener(type, e => log.push(type + ':' + (e.composedPath()[0].id || e.target.id || e.target.nodeName) + ':' + (e.propertyName || e.animationName)), true);
  // A shadow tree's own element transitioning, from the shadow tree's own sheet.
  const h1 = document.createElement('div'); h1.id = 'h1'; document.body.append(h1);
  const r1 = h1.attachShadow({ mode: 'closed' });
  r1.innerHTML = '<style>#in { opacity: 1; transition: opacity 80ms linear; } #in.on { opacity: 0.5; }</style><div id="in">in</div>';
  const inner = r1.getElementById('in'); getComputedStyle(inner).opacity;
  inner.classList.add('on');
  rows.push(['shadow element: opacity right after', getComputedStyle(inner).opacity]);
  rows.push(['shadow element: getAnimations', inner.getAnimations().map(a => a.constructor.name).join()]);
  // A slotted light child inherits from its SLOT: the slot's color changes, the child transitions `color`.
  const h2 = document.createElement('div'); h2.id = 'h2'; h2.className = 'host'; document.body.append(h2);
  const r2 = h2.attachShadow({ mode: 'open' });
  r2.innerHTML = '<style>slot { color: rgb(0, 0, 0); } slot.on { color: rgb(0, 0, 255); }</style><slot></slot>';
  const kid = document.createElement('span'); kid.id = 'kid'; kid.className = 'lite'; kid.textContent = 'k'; h2.append(kid);
  getComputedStyle(kid).color;
  r2.querySelector('slot').classList.add('on');
  rows.push(['slotted child: color right after its slot changed', getComputedStyle(kid).color]);
  rows.push(['slotted child: getAnimations', kid.getAnimations().map(a => a.constructor.name + ':' + a.transitionProperty).join()]);
  // A light child of a host with no slot is not in the flat tree: no style, no animation.
  const h3 = document.createElement('div'); h3.id = 'h3'; document.body.append(h3);
  h3.attachShadow({ mode: 'open' }).innerHTML = '<p>no slot</p>';
  const lost = document.createElement('div'); lost.id = 'lost'; lost.className = 'spin'; h3.append(lost);
  rows.push(['unslotted child: getAnimations', lost.getAnimations().length]);
  await wait(350);
  rows.push(['events', log.join(' ')]);
  rows.push(['settled: shadow element opacity', getComputedStyle(inner).opacity]);
  rows.push(['settled: slotted child color', getComputedStyle(kid).color]);
  return rows;
})()
