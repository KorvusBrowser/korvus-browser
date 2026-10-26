(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = [];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const m = (css) => { const s = document.createElement('span'); s.textContent = 'x'; s.style.cssText = 'display:inline-block;' + css; document.body.appendChild(s); const cs = getComputedStyle(s); const v = [cs.fontSize, cs.width, cs.lineHeight, s.getBoundingClientRect().height, cs.paddingLeft]; s.remove(); return v; };
  R.push(['0.5em width 10em', m('font-size:0.5em;width:10em')]);
  R.push(['0.5em lh 2', m('font-size:0.5em;line-height:2')]);
  R.push(['0.5em lh 2em', m('font-size:0.5em;line-height:2em')]);
  R.push(['0.5em pad 1em', m('font-size:0.5em;padding-left:1em')]);
  R.push(['xx-small width 10em', m('font-size:xx-small;width:10em')]);
  R.push(['4px width 10em', m('font-size:4px;width:10em')]);
  R.push(['mono 0.5em width 10em', m('font-family:monospace;font-size:0.5em;width:10em')]);
  const d = document.createElement('div'); d.style.fontSize = '0.5em'; d.innerHTML = '<span id=k style="display:inline-block;font-size:2em;width:1em">x</span>'; document.body.appendChild(d);
  const k = document.getElementById('k'); R.push(['child of 0.5em: 2em', [getComputedStyle(d).fontSize, getComputedStyle(k).fontSize, getComputedStyle(k).width]]); d.remove();
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
