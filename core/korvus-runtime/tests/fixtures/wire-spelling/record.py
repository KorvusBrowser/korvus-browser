#!/usr/bin/env python3
"""Record what stock Chrome writes on an HTTP/1.1 wire for wire_spelling_live.rs: every request's header lines,
verbatim and in order, for a page's form submissions (urlencoded, multipart, text/plain, GET, cross-site, into a
frame), a page's subresources (a stylesheet, a script, an image, a frame, fetch with the page's own header
spellings, XHR, a beacon, a CORS preflight), the headers a page sets itself (`hdr`: jQuery's XHR `Accept`, a header
set twice, whitespace, forbidden names, `setRequestHeader`'s exceptions, fetch with a record, a sequence, a Headers
object and a Request) and a page over TLS spoken as HTTP/1.1 (`tls`: no `Priority`).

Each case runs in a fresh browser, headed under the caller's display (Xvfb), with NO DevTools client attached (a
CDP client changes some answers): Chrome is started on the case's URL and stopped once the server has seen no
request for QUIET seconds. `DISPLAY=:99 [KORVUS_CHROME=<chrome>] python3 record.py RUNS > chrome-<M>.json` (the Chrome under test; default /usr/bin/google-chrome).
Ports are written `P`, a multipart boundary `----WebKitFormBoundaryX`; the runs must agree or nothing is written.
The `tls` case serves `tls-leaf.pem` (signed by `tls-ca.pem`, which the test hands korvus as KORVUS_EXTRA_CA);
Chrome takes it through `--ignore-certificate-errors-spki-list` with the CA's key hash."""
import base64, hashlib, json, os, re, shutil, signal, socket, ssl, subprocess, sys, tempfile, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))
QUIET = 2.5
KINDS = ['urlencoded', 'multipart', 'plain', 'get', 'cross', 'frame']
SUB = b'''<!doctype html><title>sub</title><link rel=stylesheet href="/r/css"><script src="/r/js"></script>
<img src="/r/img"><iframe src="/r/frame"></iframe>
<script>
onload = function () {
  fetch('/r/fetch-plain');
  fetch('/r/fetch-hdr', {headers: {'X-Foo': '1', 'x-bar': '2', 'X-BAZ': '3', 'Content-Type': 'text/plain'}});
  fetch('/r/fetch-post', {method: 'POST', body: 'a=1'});
  fetch('/r/fetch-json', {method: 'POST', body: '{}', headers: {'content-type': 'application/json'}});
  var x = new XMLHttpRequest(); x.open('POST', '/r/xhr'); x.setRequestHeader('X-Req', 'y'); x.send('b');
  navigator.sendBeacon('/r/beacon', 'z');
  fetch('http://localhost:PORT/r/cors', {headers: {'X-Cors': '1'}});
};
</script>'''
HDR = rb'''<!doctype html><title>hdr</title><script>
function xhr(path, hdrs, method, body) { return new Promise(function (res) { var x = new XMLHttpRequest(); x.open(method || 'GET', path);
  for (var i = 0; i < hdrs.length; i++) { try { x.setRequestHeader(hdrs[i][0], hdrs[i][1]); } catch (e) {} }
  x.onloadend = res; x.send(body === undefined ? null : body); }); }
function errs() { var out = [], x = new XMLHttpRequest();
  function t(f) { try { f(); out.push('ok'); } catch (e) { out.push(e.name + ': ' + e.message); } }
  t(function () { x.setRequestHeader('X-A', '1'); });
  x.open('GET', '/r/xhr-sent');
  t(function () { x.setRequestHeader('a b', '1'); });
  t(function () { x.setRequestHeader('', '1'); });
  t(function () { x.setRequestHeader('X-A', 'a\nb'); });
  t(function () { x.setRequestHeader('X-A', '\u0100'); });
  t(function () { x.setRequestHeader('X-E', ' \te\t '); });
  x.send();
  t(function () { x.setRequestHeader('X-A', '1'); });
  t(function () { x.send(); });
  return fetch('/r/xhr-errors?' + encodeURIComponent(out.join('|'))); }
var steps = [
  function () { return xhr('/r/xhr-jquery', [['Accept', 'application/json, text/javascript, */*; q=0.01'], ['X-Requested-With', 'XMLHttpRequest']]); },
  function () { return xhr('/r/xhr-plain', []); },
  function () { return xhr('/r/xhr-accept-twice', [['Accept', 'a/b'], ['accept', 'c/d']]); },
  function () { return xhr('/r/xhr-xa-twice', [['X-A', '1'], ['x-a', '2']]); },
  function () { return xhr('/r/xhr-ws', [['X-B', '  v  '], ['X-C', '\tw\t']]); },
  function () { return xhr('/r/xhr-forbidden', [['User-Agent', 'p'], ['Cookie', 'c=1'], ['Referer', 'http://x/'], ['Origin', 'http://x'],
    ['Sec-Foo', '1'], ['Proxy-Foo', '1'], ['Accept-Encoding', 'x'], ['Connection', 'close'], ['X-HTTP-Method-Override', 'TRACE'],
    ['Access-Control-Request-Private-Network', 'true'], ['X-Ok', '1']]); },
  function () { return xhr('/r/xhr-override-put', [['X-HTTP-Method-Override', 'PUT']]); },
  function () { return xhr('/r/xhr-accept-language', [['Accept-Language', 'fr']]); },
  function () { return xhr('/r/xhr-post-ct', [['Content-Type', 'application/json']], 'POST', '{}'); },
  function () { return fetch('/r/fetch-accept', {headers: {'Accept': 'application/json'}}); },
  function () { return fetch('/r/fetch-accept-language', {headers: {'Accept-Language': 'fr'}}); },
  function () { return fetch('/r/fetch-xa-twice', {headers: [['X-A', '1'], ['x-a', '2']]}); },
  function () { var h = new Headers(); h.append('Accept', 'a/b'); h.append('accept', 'c/d'); return fetch('/r/fetch-accept-twice', {headers: h}); },
  function () { return fetch('/r/fetch-headers-object', {headers: new Headers({'X-Mixed': '1', 'Accept': 'x/y'})}); },
  function () { return fetch('/r/fetch-ws', {headers: {'X-B': '  v  '}}); },
  function () { return fetch(new Request('/r/fetch-request', {headers: {'X-Req': '1', 'Accept': 'r/s'}})); },
  function () { return fetch('/r/fetch-range', {headers: {'Range': 'bytes=0-1'}}); },
  function () { return fetch('/r/fetch-forbidden', {headers: {'User-Agent': 'p', 'X-HTTP-Method-Override': 'TRACE', 'X-Ok': '1'}}); },
  errs];
steps.reduce(function (p, s) { return p.then(s, s); }, Promise.resolve());
</script>'''
TLS = b'''<!doctype html><title>tls</title><script src="/r/js"></script><img src="/r/img">
<script>
onload = function () {
  fetch('/r/tls-fetch');
  fetch('/r/tls-fetch-priority', {headers: {'priority': 'u=5'}});
  var x = new XMLHttpRequest(); x.open('GET', '/r/tls-xhr'); x.setRequestHeader('Accept', 'application/json'); x.send();
};
</script>'''


def form(kind, port):
    method, action, enctype, target, extra = 'POST', '/post?' + kind, '', '', ''
    if kind == 'multipart':
        enctype = ' enctype="multipart/form-data"'
    if kind == 'plain':
        enctype = ' enctype="text/plain"'
    if kind == 'get':
        method = 'GET'
    if kind == 'cross':
        action = 'http://localhost:%d/post?cross' % port
    if kind == 'frame':
        target, extra = ' target="fr"', '<iframe name="fr" src="/blank"></iframe>'
    return ('<!doctype html><title>form</title>%s<form method="%s" action="%s"%s%s><input name="a" value="1">'
            '<input name="b" value="x y"></form><script>onload = function () { setTimeout(function () '
            '{ document.forms[0].submit(); }, 300); };</script>' % (extra, method, action, enctype, target)).encode()


def tls_context():
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(os.path.join(HERE, 'tls-leaf.pem'), os.path.join(HERE, 'tls-leaf.key'))
    ctx.set_alpn_protocols(['http/1.1'])
    return ctx


def ca_spki():
    from cryptography import x509
    from cryptography.hazmat.primitives import serialization
    c = x509.load_pem_x509_certificate(open(os.path.join(HERE, 'tls-ca.pem'), 'rb').read())
    der = c.public_key().public_bytes(serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo)
    return base64.b64encode(hashlib.sha256(der).digest()).decode()


def serve(port, log, tls):
    def handle(c):
        f = c.makefile('rb')
        while True:
            line = f.readline()
            if not line:
                return
            req = line.decode('latin1').rstrip('\r\n')
            hdrs = []
            while True:
                h = f.readline()
                if not h or h in (b'\r\n', b'\n'):
                    break
                hdrs.append(h.decode('latin1').rstrip('\r\n'))
            n = next((int(h.split(':', 1)[1]) for h in hdrs if h.lower().startswith('content-length:')), 0)
            if n:
                f.read(n)
            log.append({'req': req, 'headers': hdrs, 't': time.time()})
            if req.startswith('OPTIONS'):
                c.sendall(b'HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\n'
                          b'Access-Control-Allow-Methods: *\r\nContent-Length: 0\r\n\r\n')
                continue
            path = req.split(' ')[1]
            p, _, q = path.partition('?')
            ct = 'text/html'
            if p == '/' and q.startswith('kind='):
                b = form(q[5:], port)
            elif p == '/sub':
                b = SUB.replace(b'PORT', str(port).encode())
            elif p == '/hdr':
                b = HDR
            elif p == '/tls':
                b = TLS
            elif p == '/r/css':
                b, ct = b'p{color:red}', 'text/css'
            elif p == '/r/js':
                b, ct = b'var a=1;', 'text/javascript'
            elif p in ('/r/frame', '/blank', '/post'):
                b = b'<!doctype html><title>x</title>x'
            elif p == '/favicon.ico':
                b, ct = b'', 'image/x-icon'
            else:
                b, ct = b'ok', 'text/plain'
            c.sendall(('HTTP/1.1 200 OK\r\nContent-Type: %s\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: %d\r\n\r\n'
                       % (ct, len(b))).encode() + b)
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(('0.0.0.0', port))
    s.listen(64)
    ctx = tls_context() if tls else None
    def conn(c):
        try:
            if ctx:
                c = ctx.wrap_socket(c, server_side=True)
            handle(c)
        except (OSError, ssl.SSLError):
            pass
        finally:
            c.close()
    def loop():
        while True:
            c, _ = s.accept()
            threading.Thread(target=conn, args=(c,), daemon=True).start()
    threading.Thread(target=loop, daemon=True).start()


def chrome(url, log, tls):
    """Chrome on `url` with no DevTools client, stopped once the server has been quiet for QUIET seconds."""
    prof = tempfile.mkdtemp(prefix='wirechr-')
    args = [os.environ.get('KORVUS_CHROME', '/usr/bin/google-chrome'), '--ozone-platform=x11', f'--user-data-dir={prof}', '--no-first-run',
            '--no-default-browser-check', '--window-size=1920,1080']
    if tls:
        args.append('--ignore-certificate-errors-spki-list=' + ca_spki())
    p = subprocess.Popen(args + [url], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    try:
        start = time.time()
        while time.time() - start < 30:
            time.sleep(0.25)
            if log and time.time() - log[-1]['t'] > QUIET and time.time() - start > 4:
                break
    finally:
        os.killpg(p.pid, signal.SIGKILL)
        p.wait()
        shutil.rmtree(prof, ignore_errors=True)


def normalize(rows, port):
    out = {}
    for r in rows:
        fix = lambda x: re.sub(r'----WebKitFormBoundary[0-9A-Za-z]{16}', '----WebKitFormBoundaryX', x.replace(str(port), 'P'))
        out.setdefault(fix(r['req']), [fix(h) for h in r['headers']])
    return out


def record():
    out = {}
    for case in KINDS + ['sub', 'hdr', 'tls']:
        s = socket.socket(); s.bind(('127.0.0.1', 0)); port = s.getsockname()[1]; s.close()
        log = []
        tls = case == 'tls'
        serve(port, log, tls)
        path = case if case in ('sub', 'hdr', 'tls') else '?kind=' + case
        chrome('%s://127.0.0.1:%d/%s' % ('https' if tls else 'http', port, path), log, tls)
        out[case] = normalize(log, port)
    return out


runs = [record() for _ in range(int(sys.argv[1]) if len(sys.argv) > 1 else 3)]
if any(r != runs[0] for r in runs[1:]):
    sys.exit('the runs disagree: %s' % json.dumps(runs, indent=1)[:4000])
print(json.dumps(runs[0], indent=1, sort_keys=True))
