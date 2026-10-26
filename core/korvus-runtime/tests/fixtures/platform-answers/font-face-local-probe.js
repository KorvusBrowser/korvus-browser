(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = [];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const names = ['Cambria Math', 'Lucida Console', 'Segoe UI Emoji', 'Segoe Fluent Icons', 'Nirmala UI', 'Leelawadee UI', 'HoloLens MDL2 Assets', 'Bahnschrift', 'Aldhabi', 'Gadugi', 'Myanmar Text', 'Javanese Text', 'Ink Free', 'Apple Symbols', 'Skia', 'Geneva', 'Luminari', 'PingFang HK Light', 'Galvji', 'InaiMathi Bold', 'Arimo', 'Ubuntu', 'Ubuntu Sans', 'Ubuntu Sans Mono', 'Cousine', 'Tinos', 'DejaVu Sans', 'Liberation Sans', 'Cantarell', 'Adwaita Sans', 'Adwaita Mono', 'Droid Sans Mono', 'Cutive Mono', 'Dancing Script', 'MS Outlook', 'ZWAdobeF', 'Liberation Mono', 'OpenSymbol', 'Source Code Pro', 'FreeMono', 'FreeSans', 'FreeSerif', 'IPAGothic', 'KacstOne', 'Loma', 'Unifont', 'WenQuanYi Zen Hei', 'Noto Color Emoji', 'Noto Sans', 'Noto Serif', 'Noto Sans Mono', 'Jomolhari', 'Caladea', 'Carlito'];
  const order = [];
  const t0 = performance.now();
  const ps = names.map((n) => { const f = new FontFace(n, 'local("' + n + '")'); return f.load().then((x) => { order.push(n); return [n, 'loaded', x === f, f.status]; }, (e) => { order.push(n); return [n, e.name + ': ' + e.message, f.status]; }); });
  const res = await Promise.all(ps);
  const dt = performance.now() - t0;
  R.push(['loaded', res.filter((r) => r[1] === 'loaded').map((r) => r[0]).join(',')]);
  R.push(['errors', [...new Set(res.filter((r) => r[1] !== 'loaded').map((r) => r[1]))].join(' | ')]);
  R.push(['settle order = call order', order.join(',') === names.join(',')]);
  R.push(['fast (<50ms)', dt < 50]);
  // the status before and after, and one more probe of a loaded and a missing face
  const f1 = new FontFace('DejaVu Sans', 'local("DejaVu Sans")'); const s0 = f1.status; const p1 = f1.load(); const s1 = f1.status;
  let r1; try { await p1; r1 = 'loaded'; } catch (e) { r1 = e.name; }
  R.push(['DejaVu again', [s0, s1, r1, f1.status, f1.family, f1.style, f1.weight, f1.display]]);
  const f2 = new FontFace('Nope Font XYZ', 'local("Nope Font XYZ")'); const p2 = f2.load(); let r2; try { await p2; r2 = 'loaded'; } catch (e) { r2 = e.name + ': ' + e.message; }
  R.push(['missing', [r2, f2.status]]);
  R.push(['document.fonts size', document.fonts.size]);
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
