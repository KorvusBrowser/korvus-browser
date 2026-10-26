# The fixture's server: the page and its three scripts, each with Server-Timing; xo-tao.js with Timing-Allow-Origin.
import http.server, socketserver, sys
PAGE = open(__file__.replace('server.py', 'server-timing.html'), 'rb').read()
ROUTES = {
    '/top.html': (b'<!doctype html><html><body><iframe src="/server-timing.html"></iframe></body></html>', 'text/html', [], False),
    '/server-timing.html': (PAGE, 'text/html', ['cfL4;desc="?proto=TCP&rtt=13092&min_rtt=12801&cwnd=242"', 'cfExtPri, db;dur=53.2;desc="Database", miss', 'a;DUR=2;dur=3;desc=tok;desc=ignored, "q";dur=1'], False),
    '/sub.js': (b'window.sub=1', 'text/javascript', ['sub;dur=1.5;desc=same'], False),
    '/xo.js': (b'window.xo=1', 'text/javascript', ['xo;dur=2'], False),
    '/xo-tao.js': (b'window.xotao=1', 'text/javascript', ['xotao;dur=3;desc="tao"'], True),
}
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        r = ROUTES.get(self.path.split('?')[0])
        if not r:
            self.send_response(404); self.end_headers(); return
        body, ty, st, tao = r
        self.send_response(200)
        self.send_header('Content-Type', ty); self.send_header('Content-Length', str(len(body)))
        for v in st: self.send_header('Server-Timing', v)
        if tao: self.send_header('Timing-Allow-Origin', '*')
        self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass
socketserver.TCPServer.allow_reuse_address = True
socketserver.TCPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
