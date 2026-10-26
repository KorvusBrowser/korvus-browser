(async () => {
  // ONE COOKIE STORE, AS CHROME 150 KEEPS IT: what a page on http://a.ck.localhost:PORT/d/page.html stores and reads
  // back -- through document.cookie, the responses of its requests, the Cookie header those requests carry, a cross-site
  // frame's document.cookie and cookieStore. `ck.localhost` is the page's registrable domain (`localhost` is a TLD to
  // the registry rules); b.ck.localhost is same-site, c.ck2.localhost and 127.0.0.1 are cross-site. Every name starts
  // with `k`, so another test's cookies on 127.0.0.1 are filtered out of the one row that could see them.
  const P = location.port;
  const A = 'http://a.ck.localhost:' + P, B = 'http://b.ck.localhost:' + P, C = 'http://c.ck2.localhost:' + P, IP = 'http://127.0.0.1:' + P;
  const R = [];
  const tick = (ms) => new Promise((r) => setTimeout(r, ms));
  const img = (u) => new Promise((r) => { const i = new Image(); i.onload = () => r('load'); i.onerror = () => r('error'); i.src = u; });
  const q = (...lines) => lines.map((l) => 'c=' + encodeURIComponent(l)).join('&');
  const text = (u, init) => fetch(u, init).then((r) => r.text(), (e) => 'reject ' + e.name);

  // 1. A document.cookie write reads no page-replaceable member (a bot-detection script D2: korvus read `window.cookieStore`).
  let reads = 0;
  const owner = Object.getOwnPropertyDescriptor(window, 'cookieStore') ? window : Window.prototype;
  const desc = Object.getOwnPropertyDescriptor(owner, 'cookieStore');
  Object.defineProperty(owner, 'cookieStore', { configurable: true, enumerable: desc.enumerable, get() { reads++; return desc.get.call(this); } });
  document.cookie = 'k_sealed=1';
  document.cookie = 'k_sealed=; max-age=0';
  Object.defineProperty(owner, 'cookieStore', desc);
  R.push(['sealed: cookieStore reads by two document.cookie writes', reads]);

  // 2. document.cookie writes, one rule each.
  const W = [
    'k_host=1',
    'k_reg=1; domain=ck.localhost',
    'k_regdot=1; domain=.ck.localhost',
    'k_regcase=1; domain=CK.Localhost',
    'k_self=1; domain=a.ck.localhost',
    'k_tld=1; domain=localhost',
    'k_sib=1; domain=b.ck.localhost',
    'k_other=1; domain=ck2.localhost',
    'k_pct=1; domain=ck%2elocalhost',
    '__Secure-k1=1',
    '__Secure-k2=1; Secure',
    '__secure-k3=1',
    '__Host-k4=1; Secure; Path=/',
    '__Host-k5=1; Secure',
    '__Host-k6=1; Secure; Path=/; Domain=a.ck.localhost',
    '__Http-k7=1; Secure; Path=/',
    '__Host-Http-k8=1; Secure; Path=/',
    '=__Host-k9',
    'k_ssnone=1; SameSite=None',
    'k_ssnone_s=1; SameSite=None; Secure',
    'k_sslax=1; SameSite=Lax',
    'k_ssstrict=1; SameSite=Strict',
    'k_ssbogus=1; SameSite=bogus',
    'k_part=1; Partitioned',
    'k_part_s=1; Secure; Partitioned',
    'k_http=1; HttpOnly',
    'k_past=1; expires=Thu, 01 Jan 1970 00:00:00 GMT',
    'k_ma0=1; max-age=0',
    'k_mabad=1; max-age=12abc',
    'k_feb30=1; expires=Mon, 30 Feb 2099 00:00:00 GMT',
    'k_ws = v w ; path=/',
    'k_tab=a\tb',
    'k_ctl=a\x01b',
    'k_eq=a=b',
    'k_empty=',
    '=k_amb=x',
    'k_bare',
    '   ',
  ];
  for (const w of W) document.cookie = w;
  R.push(['document.cookie after the writes', document.cookie]);

  // 3. Which cookie an overwrite is: the same value keeps its creation time, a new one takes a new one; a Path is
  // another cookie.
  document.cookie = 'k_o1=1'; document.cookie = 'k_o2=1'; document.cookie = 'k_o3=1';
  document.cookie = 'k_o1=1';
  document.cookie = 'k_o2=2';
  document.cookie = 'k_o3=1; path=/d/';
  R.push(['order of the overwrites', document.cookie.split('; ').filter((p) => p.startsWith('k_o')).join('; ')]);
  ['k_o1', 'k_o2', 'k_o3'].forEach((n) => { document.cookie = n + '=; max-age=0'; });
  document.cookie = 'k_o3=; max-age=0; path=/d/';

  // 4. The responses' Set-Cookie, by the request that carried them.
  const N = [
    ['img same host, Domain=ck.localhost', () => img(A + '/set?' + q('kn_img=1; Domain=ck.localhost'))],
    ['img same host, Domain=localhost', () => img(A + '/set?' + q('kn_tld=1; Domain=localhost'))],
    ['fetch same origin, credentials omit', () => text(A + '/set?' + q('kn_omit=1; Path=/'), { credentials: 'omit' })],
    ['fetch same origin, HttpOnly', () => text(A + '/set?' + q('kn_ho=1; HttpOnly; Path=/'))],
    ['fetch same site, include, Domain=ck.localhost', () => text(B + '/set?' + q('kn_b=1; Domain=ck.localhost; Path=/'), { credentials: 'include' })],
    ['fetch same site, include, host-only', () => text(B + '/set?' + q('kn_bhost=1; Path=/'), { credentials: 'include' })],
    ['img cross site, Domain=a.ck.localhost', () => img(C + '/set?' + q('kn_mis=1; Domain=a.ck.localhost; Path=/'))],
    ['img cross site, no SameSite', () => img(C + '/set?' + q('kc_lax=1; Path=/'))],
    ['img cross site, SameSite=None; Secure', () => img(C + '/set?' + q('kc_none=1; Path=/; SameSite=None; Secure'))],
    ['img cross site, SameSite=None; Secure; Partitioned', () => img(C + '/set?' + q('kc_part=1; Path=/; SameSite=None; Secure; Partitioned'))],
    ['img same host, three in one response', () => img(A + '/set?' + q('kn_m1=1; Path=/', 'kn_m2=1; Path=/', 'kn_m1=2; Path=/'))],
    ['img IP, Domain=127.0.0.1', () => img(IP + '/set?' + q('ki_dom=1; Domain=127.0.0.1; Path=/; SameSite=None; Secure'))],
    ['img IP, Domain=.127.0.0.1', () => img(IP + '/set?' + q('ki_dotdom=1; Domain=.127.0.0.1; Path=/; SameSite=None; Secure'))],
    ['img IP, Domain=0.0.1', () => img(IP + '/set?' + q('ki_part=1; Domain=0.0.1; Path=/; SameSite=None; Secure'))],
  ];
  for (const [label, f] of N) R.push([label, await f()]);
  R.push(['document.cookie after the responses', document.cookie]);

  // 5. A cross-site frame reads and writes its own partition of the store.
  const framed = await new Promise((res) => {
    addEventListener('message', function m(e) { if (e.origin === C) { removeEventListener('message', m); res(e.data); } });
    const f = document.createElement('iframe');
    f.src = C + '/frame.html';
    document.body.appendChild(f);
  });
  R.push(['cross-site frame: document.cookie before || after its writes', framed]);

  // 6. The Cookie header each request carries.
  R.push(['Cookie sent to a.ck.localhost', await text(A + '/echo')]);
  R.push(['Cookie sent to b.ck.localhost', await text(B + '/echo', { credentials: 'include' })]);
  R.push(['Cookie sent to c.ck2.localhost (cross-site)', await text(C + '/echo', { credentials: 'include' })]);
  R.push(['Cookie sent to 127.0.0.1 (cross-site)', (await text(IP + '/echo', { credentials: 'include' })).split('; ').filter((p) => p.startsWith('k')).join('; ')]);

  // 7. cookieStore reads the same store.
  const all = await cookieStore.getAll();
  R.push(['cookieStore.getAll() names', all.map((c) => c.name).join(',')]);
  for (const n of ['k_reg', 'k_host', 'k_ws', 'k_sslax', 'k_ssnone_s', 'k_part_s', 'kn_ho', '']) {
    R.push(['cookieStore.get(' + JSON.stringify(n) + ')', JSON.stringify(await cookieStore.get(n))]);
  }
  await cookieStore.set('k_cs', 'v');
  R.push(['document.cookie has the cookieStore.set cookie', document.cookie.split('; ').includes('k_cs=v')]);
  R.push(['cookieStore.get("k_cs")', JSON.stringify(await cookieStore.get('k_cs'))]);
  await cookieStore.delete('k_cs');
  R.push(['document.cookie after cookieStore.delete', document.cookie.split('; ').includes('k_cs=v')]);

  // 8. Change events for document.cookie writes: an overwrite with no visible difference is no event.
  const ev = [];
  cookieStore.addEventListener('change', (e) => ev.push(e.changed.map((c) => '+' + c.name + '=' + c.value).concat(e.deleted.map((c) => '-' + c.name + ':' + JSON.stringify(c))).join(' ')));
  await cookieStore.getAll();
  document.cookie = 'k_ev=1';
  document.cookie = 'k_ev=2';
  document.cookie = 'k_ev=2';
  document.cookie = 'k_ev=; max-age=0';
  await tick(500);
  R.push(['change events', ev.join(' | ')]);
  return R;
})()
