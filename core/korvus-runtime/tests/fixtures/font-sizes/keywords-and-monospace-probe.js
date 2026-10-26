(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = [];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const cs = (html, sel) => { const d = document.createElement('div'); d.innerHTML = html; document.body.appendChild(d); const e = d.querySelector(sel || '#t'); const v = getComputedStyle(e).fontSize; d.remove(); return v; };
  for (const k of ['xx-small', 'x-small', 'small', 'medium', 'large', 'x-large', 'xx-large', 'xxx-large', 'larger', 'smaller', 'math'])
    R.push(['kw ' + k, cs(`<span id=t style="font-size:${k}">x</span>`)]);
  for (const k of ['xx-small', 'x-small', 'small', 'medium', 'large', 'x-large', 'xx-large', 'xxx-large'])
    R.push(['mono kw ' + k, cs(`<span id=t style="font-family:monospace;font-size:${k}">x</span>`)]);
  const chains = [
    ['pre', '<pre id=t>x</pre>'], ['code', '<code id=t>x</code>'], ['kbd', '<kbd id=t>x</kbd>'], ['tt', '<tt id=t>x</tt>'], ['samp', '<samp id=t>x</samp>'], ['textarea', '<textarea id=t>x</textarea>'],
    ['mono inherit', '<span id=t style="font-family:monospace">x</span>'],
    ['mono,mono', '<span id=t style="font-family:monospace,monospace">x</span>'],
    ['"monospace" quoted', '<span id=t style="font-family:\'monospace\'">x</span>'],
    ['Courier,monospace', '<span id=t style="font-family:Courier,monospace">x</span>'],
    ['mono 1em', '<span id=t style="font-family:monospace;font-size:1em">x</span>'],
    ['mono 100%', '<span id=t style="font-family:monospace;font-size:100%">x</span>'],
    ['mono 16px', '<span id=t style="font-family:monospace;font-size:16px">x</span>'],
    ['mono 1rem', '<span id=t style="font-family:monospace;font-size:1rem">x</span>'],
    ['mono calc', '<span id=t style="font-family:monospace;font-size:calc(1em)">x</span>'],
    ['mono larger', '<span id=t style="font-family:monospace;font-size:larger">x</span>'],
    ['mono smaller', '<span id=t style="font-family:monospace;font-size:smaller">x</span>'],
    ['px parent > mono', '<div style="font-size:20px"><span id=t style="font-family:monospace">x</span></div>'],
    ['em parent > mono', '<div style="font-size:1.25em"><span id=t style="font-family:monospace">x</span></div>'],
    ['% parent > mono', '<div style="font-size:125%"><span id=t style="font-family:monospace">x</span></div>'],
    ['kw parent > mono', '<div style="font-size:large"><span id=t style="font-family:monospace">x</span></div>'],
    ['mono > sans child', '<div style="font-family:monospace"><span id=t style="font-family:sans-serif">x</span></div>'],
    ['mono > sans child em', '<div style="font-family:monospace"><span id=t style="font-family:sans-serif;font-size:1em">x</span></div>'],
    ['mono > mono child', '<div style="font-family:monospace"><span id=t>x</span></div>'],
    ['mono > em child', '<div style="font-family:monospace"><span id=t style="font-size:2em">x</span></div>'],
    ['pre > code', '<pre><code id=t>x</code></pre>'],
    ['small elem', '<small id=t>x</small>'], ['big elem', '<big id=t>x</big>'],
    ['small in small', '<small><small id=t>x</small></small>'],
    ['larger x3', '<span style="font-size:larger"><span style="font-size:larger"><span id=t style="font-size:larger">x</span></span></span>'],
    ['small > larger', '<span style="font-size:small"><span id=t style="font-size:larger">x</span></span>'],
    ['small > smaller', '<span style="font-size:small"><span id=t style="font-size:smaller">x</span></span>'],
    ['xx-small > smaller', '<span style="font-size:xx-small"><span id=t style="font-size:smaller">x</span></span>'],
    ['15px > larger', '<span style="font-size:15px"><span id=t style="font-size:larger">x</span></span>'],
    ['font size=1', '<font id=t size=1>x</font>'], ['font size=2', '<font id=t size=2>x</font>'], ['font size=3', '<font id=t size=3>x</font>'], ['font size=5', '<font id=t size=5>x</font>'], ['font size=7', '<font id=t size=7>x</font>'],
    ['h1', '<h1 id=t>x</h1>'], ['h4', '<h4 id=t>x</h4>'], ['h6', '<h6 id=t>x</h6>'], ['sub', '<sub id=t>x</sub>'],
    ['mono h1', '<h1 id=t style="font-family:monospace">x</h1>'],
    ['code in h1', '<h1><code id=t>x</code></h1>'],
  ];
  for (const [n, h] of chains) R.push([n, cs(h)]);
  // a width consequence: code text width
  const d = document.createElement('div'); d.innerHTML = '<code id=c>abcdefghij</code>'; document.body.appendChild(d);
  R.push(['code width', document.getElementById('c').getBoundingClientRect().width]); d.remove();
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
