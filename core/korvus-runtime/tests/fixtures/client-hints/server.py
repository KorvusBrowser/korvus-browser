#!/usr/bin/env python3
"""The Client Hints fixture's server (tools-only; korvus's test runs its own in Rust with the same routes).

    python3 server.py PORT OUT.json

Every request is recorded -- method, path, the header NAMES in wire order (lowercased) and the value of each
client hint -- and OUT.json is written when /ch/done arrives (the follow-up navigation's page asks for it).
"""
import http.server, json, sys

PAGE = open(__file__.rsplit('/', 1)[0] + '/client-hints.html', 'rb').read()
ACCEPT_CH = ('Sec-CH-UA-Full-Version-List, Sec-CH-UA-Arch, Sec-CH-UA-Bitness, Sec-CH-UA-Model, '
             'Sec-CH-UA-Platform-Version, Sec-CH-UA-Full-Version, Sec-CH-UA-WoW64, Sec-CH-UA-Form-Factors, UA-Arch')
CRITICAL_CH = 'Sec-CH-UA-Full-Version-List, Sec-CH-UA-Arch'
LOG = []


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def record(self):
        hs = [(k.lower(), v) for k, v in self.headers.items()]
        LOG.append({'m': self.command, 'host': self.headers.get('host', '').split(':')[0], 'path': self.path,
                    'names': [k for k, _ in hs],
                    'hints': {k: v for k, v in hs if k.startswith('sec-ch-') or k in ('cookie',)}})

    def send(self, code, ctype, body, extra=()):
        self.send_response(code)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(body)))
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.record()
        p = self.path.split('?')[0]
        if p == '/ch/':
            self.send(200, 'text/html', PAGE, [('Accept-CH', ACCEPT_CH), ('Critical-CH', CRITICAL_CH),
                                              ('Set-Cookie', 'k=1; Path=/')])
        elif p == '/ch/next':
            self.send(200, 'text/html', b"<!doctype html><link rel=icon href=\"data:,\"><script>fetch('/ch/done')</script>")
        elif p == '/ch/done':
            self.send(200, 'text/plain', b'ok')
            json.dump(LOG, open(sys.argv[2], 'w'), indent=1)
        elif p == '/ch/frame.html':
            self.send(200, 'text/html', b'<!doctype html><p>f</p>')
        elif p.endswith('.js'):
            self.send(200, 'text/javascript', b'1')
        else:
            self.send(200, 'text/plain', b'ok')

    def do_POST(self):
        self.rfile.read(int(self.headers.get('content-length', 0)))
        self.record()
        self.send(200, 'text/plain', b'ok')

    def log_message(self, *a):
        pass


http.server.ThreadingHTTPServer(('0.0.0.0', int(sys.argv[1])), H).serve_forever()
