(async () => {
  const rows = [];
  const st = document.createElement('style');
  st.textContent = `@keyframes kf { from { opacity: 0.25; } to { opacity: 0.75; } } .spin { animation: kf 80ms linear; }`;
  document.head.appendChild(st);
  const log = [];
  for (const type of ['transitionrun', 'transitionstart', 'transitionend', 'transitioncancel', 'animationstart', 'animationend', 'animationcancel'])
    document.addEventListener(type, e => log.push(type + ':' + e.target.id + ':' + (e.propertyName || e.animationName)), true);
  // A closed shadow tree's element transitioning: its events are not composed, so the document never sees them.
  const h1 = document.createElement('div'); h1.id = 'h1'; document.body.append(h1);
  const r1 = h1.attachShadow({ mode: 'closed' });
  r1.innerHTML = '<style>#in { opacity: 1; transition: opacity 80ms linear; } #in.on { opacity: 0.5; }</style><div id="in">in</div>';
  const inner = r1.getElementById('in'); const seen = [];
  for (const type of ['transitionrun', 'transitionend']) inner.addEventListener(type, e => seen.push(type));
  getComputedStyle(inner).opacity; inner.classList.add('on');
  rows.push(['shadow element: opacity right after', getComputedStyle(inner).opacity]);
  rows.push(['shadow element: getAnimations', inner.getAnimations().map(a => a.constructor.name).join()]);
  // A light child of a host with no slot is not in the flat tree: no animation.
  const h3 = document.createElement('div'); h3.id = 'h3'; document.body.append(h3);
  h3.attachShadow({ mode: 'open' }).innerHTML = '<p>no slot</p>';
  const lost = document.createElement('div'); lost.id = 'lost'; lost.className = 'spin'; h3.append(lost);
  const deep = document.createElement('div'); deep.id = 'deep'; deep.className = 'spin'; lost.append(deep);
  rows.push(['unslotted child: getAnimations', lost.getAnimations().length]);
  rows.push(['below an unslotted child: getAnimations', deep.getAnimations().length]);
  // The same element given a slot to go to: now it is in the flat tree and animates.
  const h4 = document.createElement('div'); h4.id = 'h4'; document.body.append(h4);
  h4.attachShadow({ mode: 'open' }).innerHTML = '<slot></slot>';
  const found = document.createElement('div'); found.id = 'found'; found.className = 'spin'; h4.append(found);
  rows.push(['slotted child: getAnimations', found.getAnimations().length]);
  await new Promise(r => setTimeout(r, 350));
  rows.push(['events at the document', log.join(' ')]);
  rows.push(['events at the shadow element', seen.join(' ')]);
  return rows;
})()
