(async () => {
  // rows a later batch owns (see the test's doc), and the page's own origin as ORIGIN
  const SKIP = ['measures as the font'];
  const SKIP_PREFIX = [];
  const R = await ((async () => {
  const R = [];
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const settleOrder = async (f) => {
    const order = []; const p = f.load(); const s1 = f.status;
    p.then(() => order.push('API'), () => order.push('API!'));
    Promise.resolve().then(() => order.push('micro'));
    const mc = new MessageChannel(); mc.port1.onmessage = () => order.push('msg'); mc.port2.postMessage(0);
    setTimeout(() => order.push('t0'), 0);
    await wait(50);
    return [s1, f.status, order.join(' ')];
  };
  R.push(['found', await settleOrder(new FontFace('A1', 'local("DejaVu Sans")'))]);
  R.push(['missing', await settleOrder(new FontFace('A2', 'local("Nope XYZ")'))]);
  R.push(['missing then found', await settleOrder(new FontFace('A3', 'local("Nope XYZ"), local("Liberation Mono")'))]);
  R.push(['postscript name', await settleOrder(new FontFace('A4', 'local("DejaVuSans")'))]);
  R.push(['case', await settleOrder(new FontFace('A5', 'local("dejavu sans")'))]);
  R.push(['bold full name', await settleOrder(new FontFace('A6', 'local("DejaVu Sans Bold")'))]);
  R.push(['family only', await settleOrder(new FontFace('A7', 'local("Ubuntu Sans")'))]);
  R.push(['unquoted', await settleOrder(new FontFace('A8', 'local(DejaVu Sans)'))]);
  R.push(['loaded promise same', (() => { const f = new FontFace('A9', 'local("DejaVu Sans")'); const p = f.load(); return [p === f.loaded, p === f.load()]; })()]);
  // a found local face in use: its family measures as the font
  {
    const f = new FontFace('MyLocal', 'local("Liberation Mono")'); await f.load(); document.fonts.add(f);
    const c = document.createElement('canvas').getContext('2d');
    c.font = '20px MyLocal'; const a = c.measureText('iiiii').width;
    c.font = '20px "Liberation Mono"'; const b = c.measureText('iiiii').width;
    c.font = '20px serif'; const s = c.measureText('iiiii').width;
    R.push(['measures as the font', [a === b, a !== s, document.fonts.check('20px MyLocal'), document.fonts.size]]);
  }
  R.push(['fonts.check local names', [document.fonts.check('12px "DejaVu Sans"'), document.fonts.check('12px "Nope XYZ"')]]);
  return R;
})());
  const kept = R.filter((r) => !SKIP.includes(r[0]) && !SKIP_PREFIX.some((p) => String(r[0]).startsWith(p)));
  return JSON.parse(JSON.stringify(kept).split(location.origin).join('ORIGIN'));
})()
