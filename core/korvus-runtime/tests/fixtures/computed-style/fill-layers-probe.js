(() => {
  // The fill-layer family as getComputedStyle and the CSSOM answer it: [declaration, property, value] for each
  // declaration and each property read; a sheet's rule as [rule text, member, value].
  const R = [];
  const norm = (v) => String(v).split(location.origin).join('ORIGIN');
  const props = ['background-position', 'background-position-x', 'background-position-y', 'background-size', 'background-repeat',
    'background-attachment', 'background-origin', 'background-clip', 'background-image', 'background-color', 'background',
    'mask-position', '-webkit-mask-position-x', '-webkit-mask-position-y', 'mask-size', 'mask-repeat', 'mask-image',
    'border-spacing', '-webkit-border-horizontal-spacing', '-webkit-border-vertical-spacing', 'page', 'position-anchor'];
  const read = (tag, decl) => {
    const el = document.createElement(tag);
    el.setAttribute('style', decl);
    document.body.appendChild(el);
    const cs = getComputedStyle(el);
    for (const p of props) R.push([tag + ' ' + decl, p, norm(cs.getPropertyValue(p))]);
    el.remove();
  };
  for (const d of [
    '',
    'background-position: right 5px bottom',
    'background-position: center',
    'background-position: top',
    'background-position: left 10% top 1em',
    'background-position: bottom 10% right',
    'background-position: 1em 2em',
    'background-position: right 25% center',
    'background-image: url(a.png), url(b.png); background-position: 1px 2px, right 3px top',
    'background: url(a.png) no-repeat, url(b.png) red',
    'background: red url(x.png) no-repeat center / 10px 20px fixed content-box padding-box',
    'background: url(a.png) left 5px top 10px / 50% auto repeat-x',
    'background: url(a.png) top left / cover local',
    'background: url(a.png) 10px 20px no-repeat',
    'background-image: url(a.png), url(b.png), url(c.png); background-repeat: no-repeat, repeat-x',
    'background-image: url(a.png); background-size: 1px, 2px, 3px',
    'background-repeat: repeat no-repeat',
    'background-repeat: no-repeat repeat',
    'background-repeat: space space',
    'background-size: 1em auto',
    'background-size: auto 2em',
    'background-size: auto auto',
    'mask-position: right 2px bottom 3px',
    'mask-image: url(a.png), url(b.png); mask-position: 1px 2px',
    'mask-size: 2em auto',
    'border-spacing: 2px 4px',
    'border-spacing: 1em',
    'page: chapter',
    'position-anchor: --a',
  ]) read('div', d);
  read('table', '');
  read('table', 'border-spacing: 0');
  // declared: a sheet's rules read back
  const st = document.createElement('style');
  st.textContent = 'a { background-position: right 5px bottom }\n' +
    'b { background: url(a.png) no-repeat, url(b.png) red }\n' +
    'i { background: url(a.png) center / cover }\n' +
    'u { mask-position: 1px 2px, 3px 4px }\n' +
    's { background-position-x: 10px; background-position-y: top }';
  document.head.appendChild(st);
  for (const rule of st.sheet.cssRules) {
    const key = rule.selectorText;
    R.push([key, 'cssText', norm(rule.cssText)]);
    for (const m of ['backgroundPosition', 'backgroundPositionX', 'backgroundPositionY', 'background', 'maskPosition', 'length', '0', '1']) {
      R.push([key, m, norm(rule.style[m])]);
    }
  }
  st.remove();
  return R;
})()
