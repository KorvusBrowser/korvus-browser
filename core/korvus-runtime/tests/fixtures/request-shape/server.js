// Chrome's side of `tests/request_shape_live.rs`: request-shape.html over HTTP/2 + TLS on two ports of
// localhost (one site, two origins), every request recorded as the Rust test's server records it, plus the
// `priority` field Chrome sends on h2. Usage:
//   node server.js PORT_A PORT_B OUT.json KEY.pem CERT.pem
//   google-chrome --user-data-dir=$(mktemp -d) --ignore-certificate-errors-spki-list=<the cert's SPKI hash> \
//     https://localhost:PORT_A/rs/set
// OUT.json is written when the page asks for /rs/done.
const http2 = require('http2'), fs = require('fs');
const [A, B] = [+process.argv[2], +process.argv[3]], out = process.argv[4];
const tls = { key: fs.readFileSync(process.argv[5]), cert: fs.readFileSync(process.argv[6]) };
const PAGE = fs.readFileSync(__dirname + '/request-shape.html', 'utf8').split('{{B}}').join('https://localhost:' + B);
const LOG = [];
function answer(port, st, h, raw) {
  const names = [];
  for (let i = 0; i < raw.length; i += 2) names.push(raw[i].toLowerCase());
  const path = h[':path'], m = h[':method'], cookie = h['cookie'] || '';
  LOG.push({ m, port: port === A ? 'A' : 'B', path, names, mode: h['sec-fetch-mode'] || null, origin: 'origin' in h,
             rs7: /(^|; )rs7=1/.test(cookie), rsz: /(^|; )rsz=1/.test(cookie), priority: h['priority'] || null });
  const o = h['origin'];
  const cors = { 'access-control-allow-origin': o || '*', 'access-control-allow-credentials': 'true', 'access-control-allow-headers': 'x-k' };
  if (m === 'OPTIONS') { st.respond({ ':status': 204, ...cors }); st.end(); return; }
  if (path === '/rs/set') { st.respond({ ':status': 302, 'location': '/rs/', 'set-cookie': 'rs7=1; Path=/rs' }); st.end(); return; }
  if (path === '/rs/done') fs.writeFileSync(out, JSON.stringify(LOG, null, 1));
  const [ctype, body] = path === '/rs/' ? ['text/html', PAGE] : path.endsWith('.js') ? ['text/javascript', '1']
    : path.endsWith('.css') ? ['text/css', 'a{}'] : ['text/plain', 'ok'];
  const extra = path === '/rs/f-setz' ? { 'set-cookie': 'rsz=1; Path=/rs' } : {};
  st.respond({ ':status': 200, 'content-type': ctype, 'cache-control': 'no-store', ...cors, ...extra });
  st.end(body);
}
for (const port of [A, B]) {
  const s = http2.createSecureServer(tls);
  s.on('stream', (st, h, flags, raw) => answer(port, st, h, raw));
  s.listen(port, '::');
}
