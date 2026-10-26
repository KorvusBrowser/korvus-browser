(async function () {
  // ABPROG batch 4, wave 2: what a page reads of geolocation, getUserMedia and a form's navigate event. A row is
  // [case, answer]; a time is bucketed (`<50`, `<400`, `<1500`, `none`) because Chrome's own varies run to run.
  const out = [];
  const bucket = (ms) => (ms < 50 ? '<50' : ms < 400 ? '<400' : ms < 1500 ? '<1500' : '>=1500');
  const settle = (p, ms) => Promise.race([p, new Promise((r) => setTimeout(() => r('none'), ms))]);
  async function timed(name, f, ms) {
    const t0 = performance.now();
    const r = await settle(f().then((v) => 'ok ' + v, (e) => (e && e.name ? e.name + ': ' + e.message : 'rejected ' + e)), ms);
    out.push([name, r + ' | ' + (r === 'none' ? 'none' : bucket(performance.now() - t0))]);
  }
  try { out.push(['geolocation permission', (await navigator.permissions.query({ name: 'geolocation' })).state]); } catch (e) { out.push(['geolocation permission', e.name]); }
  const geo = (opts) => () => new Promise((ok, bad) => navigator.geolocation.getCurrentPosition(
    (p) => ok('position'), (e) => bad({ name: 'PositionError', message: e.code + ' ' + e.message }), opts));
  await timed('getCurrentPosition timeout 0', geo({ timeout: 0 }), 1500);
  await timed('getCurrentPosition timeout 100', geo({ timeout: 100 }), 1500);
  await timed('getCurrentPosition maximumAge Infinity timeout 0', geo({ maximumAge: Infinity, timeout: 0 }), 1500);
  try { out.push(['watchPosition ids', [navigator.geolocation.watchPosition(() => {}), navigator.geolocation.watchPosition(() => {})].join(',')]); } catch (e) { out.push(['watchPosition ids', e.name]); }
  await timed('getUserMedia audio', () => navigator.mediaDevices.getUserMedia({ audio: true }).then((s) => s.getTracks().length), 3000);
  await timed('getUserMedia video', () => navigator.mediaDevices.getUserMedia({ video: true }).then((s) => s.getTracks().length), 3000);
  await timed('getUserMedia nothing', () => navigator.mediaDevices.getUserMedia({}), 3000);
  await timed('getUserMedia no argument', () => navigator.mediaDevices.getUserMedia(), 3000);
  await timed('getDisplayMedia', () => navigator.mediaDevices.getDisplayMedia({ video: true }), 1500);
  // a form submission's navigate event, intercepted so the page stays
  const log = [];
  const form = document.createElement('form');
  form.method = 'post';
  form.action = location.pathname + '?formprobe';
  form.innerHTML = '<input name=a value=1><input name=b value=two><button name=go value=x>go</button>';
  document.body.append(form);
  form.addEventListener('formdata', (e) => log.push('formdata ' + [...e.formData.keys()].join('+')));
  form.addEventListener('submit', () => log.push('submit'));
  const onnav = (e) => {
    log.push(['navigate', e.navigationType, e.canIntercept, e.userInitiated, e.hashChange, e.destination.url.replace(location.origin, ''),
      e.formData ? [...e.formData.entries()].map((x) => x.join('=')).join('&') : String(e.formData),
      e.sourceElement ? e.sourceElement.nodeName : String(e.sourceElement), e.downloadRequest, e.info].join('|'));
    if (e.canIntercept) e.intercept();
  };
  navigation.addEventListener('navigate', onnav);
  form.querySelector('button').click();
  await new Promise((r) => setTimeout(r, 300));
  navigation.removeEventListener('navigate', onnav);
  out.push(['form submit', log.join(' ; ')]);
  form.remove();
  return out;
})()
