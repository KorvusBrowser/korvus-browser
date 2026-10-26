(() => {
  // A font-detection probe as fingerprint scripts write it (LEAN latency b0): one off-screen absolutely positioned span,
  // its font-family rewritten, its size read after each.
  // a document to carry: 200 flex rows of text, laid out once and then left alone
  const block = document.createElement('div');
  block.innerHTML = Array.from({ length: 200 }, (_, i) => '<div style="display: flex; padding: 2px"><span>row ' + i + '</span> some text <b>bold</b></div>').join('');
  document.body.appendChild(block);
  void block.offsetHeight;
  const span = document.createElement('span');
  span.style.cssText = 'position: absolute; left: -9999px; font-size: 72px; font-style: normal; font-weight: normal; letter-spacing: normal; line-break: auto; line-height: normal; text-transform: none; text-align: left; text-decoration: none; text-shadow: none; white-space: normal; word-break: normal; word-spacing: normal';
  span.textContent = 'mmmmmmmmmmlli';
  document.body.appendChild(span);
  const families = ['monospace', 'serif', 'sans-serif', 'cursive', 'fantasy', 'Arial', 'Courier New', 'Times New Roman', 'DejaVu Sans', 'DejaVu Serif', 'Liberation Mono', 'Ubuntu', 'Noto Sans', 'No Such Font', 'No Such Font, monospace', 'No Such Font, serif'];
  const rows = families.map(f => { span.style.fontFamily = f; return [f, span.offsetWidth, span.offsetHeight]; });
  span.remove();
  return rows;
})()
