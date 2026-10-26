// A frame document's own parser script writes: the markup goes in at the insertion point, after the script, at
// once -- what it wrote is in the tree before the write returns, a written inline script runs, and a written
// external one runs once loaded. Naver's ad frames write their creative this way (`gfp-display-safeframe.js`).
(async () => {
  const out = [];
  const f = document.createElement('iframe');
  document.body.appendChild(f);
  const d = f.contentDocument;
  d.open();
  d.write('<!doctype html><body><p id=a>a</p><script>document.write("<p id=w>written</p><script>window.ran = (window.ran || 0) + 1<\\/script>"); window.after = !!document.getElementById("w");<\/script><p id=b>b</p>'
    + '<script>document.write("<script src=\'data:text/javascript,window.ext%20%3D%201\'><\\/script><p id=c>c</p>")<\/script><p id=d>d</p></body>');
  d.close();
  await new Promise(r => setTimeout(r, 200));
  out.push([...d.body.children].map(e => e.id || e.tagName).join(','));
  out.push(f.contentWindow.after, f.contentWindow.ran, f.contentWindow.ext);
  return out;
})()
