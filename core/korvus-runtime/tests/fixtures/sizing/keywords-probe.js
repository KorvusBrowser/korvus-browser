(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = ['min-width:max-content', 'max-width:min-content'];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const KW = ['auto', 'fit-content', '-webkit-fit-content', 'min-content', '-webkit-min-content', 'max-content', '-webkit-max-content', 'fit-content(50px)', 'fit-content(30%)', 'stretch', '-webkit-fill-available', '-moz-available', 'fill-available', 'intrinsic', 'min-intrinsic', '100px', '50%'];
  for (const kw of KW) {
    const s = document.createElement('span'); s.style.width = kw;
    R.push(['parse ' + kw, s.style.width]);
  }
  const text = 'aaaa bbbbbbbb cc dddddd eeeeeeeeeee ff';
  for (const disp of ['block', 'inline-block', 'table', 'flex']) {
    for (const cw of ['0px', '100px', '1000px']) {
      for (const kw of KW) {
        const d = document.createElement('div'); d.style.cssText = 'width:' + cw + ';font:16px/1 Arial;margin:0;padding:0';
        const s = document.createElement('span'); s.textContent = text;
        s.style.cssText = 'display:' + disp + ';width:' + kw + ';margin:0 3px;padding:0 2px;border:0 solid;border-width:0 1px';
        d.appendChild(s); document.body.appendChild(d);
        const r = s.getBoundingClientRect();
        R.push([disp + ' in ' + cw + ' width:' + kw, [r.width, r.height, getComputedStyle(s).width]]);
        d.remove();
      }
    }
  }
  // floats and abspos, and min/max-width keywords
  for (const pos of ['float:left', 'position:absolute']) {
    for (const kw of ['auto', 'fit-content', 'min-content', 'max-content', 'stretch', '-webkit-fill-available']) {
      const d = document.createElement('div'); d.style.cssText = 'position:relative;width:100px;height:50px;font:16px/1 Arial';
      const s = document.createElement('span'); s.textContent = text; s.style.cssText = pos + ';width:' + kw;
      d.appendChild(s); document.body.appendChild(d);
      const r = s.getBoundingClientRect(); R.push([pos + ' width:' + kw, [r.width, r.height]]); d.remove();
    }
  }
  for (const mm of ['min-width:max-content', 'max-width:min-content', 'max-width:fit-content', 'min-width:fit-content', 'max-width:max-content', 'min-width:min-content']) {
    const d = document.createElement('div'); d.style.cssText = 'width:100px;font:16px/1 Arial';
    const s = document.createElement('div'); s.textContent = text; s.style.cssText = mm;
    d.appendChild(s); document.body.appendChild(d);
    const r = s.getBoundingClientRect(); R.push([mm, [r.width, r.height, getComputedStyle(s).minWidth, getComputedStyle(s).maxWidth]]); d.remove();
  }
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
