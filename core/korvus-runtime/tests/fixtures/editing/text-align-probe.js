(async () => {
  // text-align as getComputedStyle reads it: match-parent is no value in Chrome 150 (the UA's li rule and a page's
  // own are dropped), a th's UA -internal-center is center unless its parent aligns otherwise.
  const R = [];
  const mk = (html) => { const d = document.createElement('div'); d.innerHTML = html; document.body.append(d); return d; };
  const cases = [
    ['ul li', '<ul><li>x</li></ul>', 'li'],
    ['ul center li', '<ul style="text-align:center"><li>x</li></ul>', 'li'],
    ['rtl ul li', '<ul dir="rtl"><li>x</li></ul>', 'li'],
    ['rtl start parent, li ltr', '<ul dir="rtl"><li dir="ltr">x</li></ul>', 'li'],
    ['end parent', '<div style="text-align:end"><p style="text-align:match-parent">x</p></div>', 'p'],
    ['explicit match-parent', '<div style="text-align:right"><span style="text-align:match-parent">x</span></div>', 'span'],
    ['th', '<table><tr><th>x</th></tr></table>', 'th'],
    ['th in center', '<table style="text-align:center"><tr><th>x</th></tr></table>', 'th'],
    ['inline start', '<p style="text-align:start">x</p>', 'p'],
  ];
  for (const [label, html, sel] of cases) {
    const d = mk(html);
    const e = d.querySelector(sel);
    R.push([label, getComputedStyle(e).textAlign]);
    d.remove();
  }
  return R;
})()
