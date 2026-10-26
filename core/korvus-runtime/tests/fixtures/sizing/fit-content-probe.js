(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = [];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const t = (name, f) => { try { R.push([name, f()]); } catch (e) { R.push([name, 'ERR ' + e.name + ': ' + e.message]); } };
  const SPAN = 'display:inline-block;width:fit-content;margin:0;padding:0;border:0;white-space:pre;font-family:monospace;font-size:16px;font-kerning:none;font-variant-ligatures:none;letter-spacing:normal;word-spacing:normal;line-height:1;';
  const BOX = 'position:fixed;left:-10000px;top:0;width:0;height:0;overflow:hidden;visibility:hidden;opacity:0;pointer-events:none;z-index:-1;';
  const measure = (box, span, text) => {
    const d = document.createElement('div'); d.style.cssText = box; document.body.appendChild(d);
    const s = document.createElement('span'); s.textContent = text; s.style.cssText = span; d.appendChild(s);
    const r = s.getBoundingClientRect(); const o = [r.x, r.y, r.width, r.height, s.offsetWidth, s.offsetHeight, s.scrollWidth, s.clientWidth];
    d.remove(); return o;
  };
  t('challenge probe', () => measure(BOX, SPAN, 'abababababababababababab'));
  for (const w of ['fit-content', 'max-content', 'min-content', 'auto', '-webkit-fit-content', 'fit-content(50px)', '100px', '50%'])
    t('width ' + w + ' in 0-wide box', () => measure(BOX, SPAN.replace('width:fit-content', 'width:' + w), 'abab ab abab'));
  for (const box of ['width:0;', 'width:0;overflow:hidden;', 'position:fixed;width:0;', 'position:absolute;left:-10000px;width:0;', 'width:0;visibility:hidden;', 'width:20px;', 'display:flex;width:0;', 'float:left;width:0;'])
    t('box ' + box, () => measure(box, SPAN, 'abababababababababababab'));
  for (const disp of ['inline', 'block', 'inline-block', 'inline-flex', 'table', 'inline-table', 'flex'])
    t('display ' + disp, () => measure(BOX, SPAN.replace('display:inline-block', 'display:' + disp), 'ab ab'));
  for (const ff of ['monospace', 'serif', 'sans-serif', '"DejaVu Sans"', '"Liberation Mono"', '"Liberation Sans"', '"Noto Color Emoji"', 'system-ui', 'nonexistentfont123'])
    t('font ' + ff, () => measure(BOX, SPAN.replace('font-family:monospace', 'font-family:' + ff), 'abababababababababababab'));
  t('nowrap text', () => measure(BOX, SPAN.replace('white-space:pre', 'white-space:nowrap'), 'ab ab ab ab'));
  t('normal text', () => measure(BOX, SPAN.replace('white-space:pre', 'white-space:normal'), 'ab ab ab ab'));
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
