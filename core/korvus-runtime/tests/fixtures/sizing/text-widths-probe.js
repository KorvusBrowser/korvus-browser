(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = [];
  const SKIP_PREFIX = ['"Noto Sans Mono CJK'];
  const R = await ((async () => {
  const R = [];
  const span = (text, css) => { const s = document.createElement('span'); s.textContent = text; s.style.cssText = css; document.body.appendChild(s); const w = s.getBoundingClientRect().width; s.remove(); return w; };
  const c = document.createElement('canvas').getContext('2d');
  for (const ff of ['monospace', '"Noto Sans Mono CJK KR"', '"Noto Sans Mono CJK JP"', 'sans-serif', '"Noto Sans CJK KR"']) {
    for (const n of [1, 2, 24, 240]) {
      const t = 'ab'.repeat(n);
      R.push([ff + ' x' + n, [span(t, 'white-space:pre;font:16px ' + ff), span(t, 'white-space:pre;font:16px ' + ff + ';font-kerning:none;font-variant-ligatures:none'), (c.font = '16px ' + ff, c.measureText(t).width)]]);
    }
  }
  R.push(['mono 13px', span('ab'.repeat(24), 'white-space:pre;font-family:monospace')]);
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
