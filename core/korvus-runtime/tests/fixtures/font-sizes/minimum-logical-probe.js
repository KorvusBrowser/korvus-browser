(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = [];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const one = (css, text) => { const s = document.createElement('span'); s.style.cssText = css; s.textContent = text || 'abababab'; document.body.appendChild(s); const v = [getComputedStyle(s).fontSize, s.getBoundingClientRect().width, s.getBoundingClientRect().height]; s.remove(); return v; };
  for (const v of ['1px', '5px', '6px', '8px', '9px', '9.5px', '10px', '0.5em', '50%', '0.3rem', 'calc(5px)', 'xx-small', '0px'])
    R.push(['size ' + v, one('font-size:' + v + ';font-family:Arial')]);
  const c = document.createElement('canvas').getContext('2d');
  for (const f of ['5px Arial', '8px Arial', '9px Arial'])
    R.push(['canvas ' + f, (c.font = f, [c.font, c.measureText('abababab').width])]);
  R.push(['lang', [navigator.language, document.documentElement.lang]]);
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
