#!/usr/bin/env python3
"""record-x11.py VARIANT OUT (VARIANT button / single / two): stock Chrome 150, headed on $DISPLAY (a PRIVATE Xvfb, no window
manager), NO DevTools; page.html served
locally; once the page reports /ready, REAL X11 XTest input clicks the field and types the fixed sequence
  Shift+a, b, Shift+2, Ctrl+a, x, Return
and the page POSTs its event rows to /report, written to OUT. Chrome's process group is the recorded PID.
--password-store=basic and --simulate-outdated-no-au keep a keyring prompt and the "cannot update" bubble from taking the
focus after the first key (the bubble did, seen on a screenshot: every later key went to it). The env knobs (SEQ, NOPIN, SNAP, XF, QS)
are the debugging aids that found that; the fixtures were recorded with NOPIN=1 and nothing else."""
import ctypes, ctypes.util, http.server, os, signal, socket, socketserver, subprocess, sys, tempfile, threading, time
VARIANT, OUT = sys.argv[1], sys.argv[2]
HERE = os.path.dirname(os.path.abspath(__file__))
PAGE = open(os.path.join(HERE, 'page.html'), 'rb').read()
ready = threading.Event(); done = threading.Event(); report = {}
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        b, ty = (PAGE, 'text/html; charset=utf-8') if self.path.startswith('/?') or self.path == '/' else (b'', 'text/plain')
        if self.path == '/ready': ready.set()
        self.send_response(200); self.send_header('Content-Type', ty); self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_POST(self):
        n = int(self.headers.get('Content-Length', '0')); body = self.rfile.read(n)
        self.send_response(200); self.send_header('Content-Length', '0'); self.end_headers()
        if self.path == '/report': report['body'] = body; done.set()
class S(socketserver.ThreadingMixIn, http.server.HTTPServer): daemon_threads = True
srv = S(('127.0.0.1', 0), H); port = srv.server_address[1]
threading.Thread(target=srv.serve_forever, daemon=True).start()

X11 = ctypes.CDLL(ctypes.util.find_library("X11")); XT = ctypes.CDLL(ctypes.util.find_library("Xtst"))
X11.XOpenDisplay.restype = ctypes.c_void_p; X11.XOpenDisplay.argtypes = [ctypes.c_char_p]
X11.XFlush.argtypes = [ctypes.c_void_p]
X11.XStringToKeysym.restype = ctypes.c_ulong; X11.XStringToKeysym.argtypes = [ctypes.c_char_p]
X11.XKeysymToKeycode.restype = ctypes.c_ubyte; X11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
XT.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]
XT.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
XT.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
D = X11.XOpenDisplay(os.environ['DISPLAY'].encode()); assert D
def kc(name): return X11.XKeysymToKeycode(D, X11.XStringToKeysym(name.encode()))
X11.XGetInputFocus.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_int)]
def xfocus():
    w = ctypes.c_ulong(); r = ctypes.c_int(); X11.XGetInputFocus(D, ctypes.byref(w), ctypes.byref(r)); return hex(w.value)
def key(name, down): XT.XTestFakeKeyEvent(D, kc(name), 1 if down else 0, 0); X11.XFlush(D); time.sleep(float(os.environ.get("KGAP", "0.09")))

prof = tempfile.mkdtemp(prefix='c7keys-', dir='/tmp/claude-1000')
chrome = subprocess.Popen(['/usr/bin/google-chrome', '--ozone-platform=x11', f'--user-data-dir={prof}', '--no-first-run',
                           '--no-default-browser-check', '--password-store=basic', '--simulate-outdated-no-au=Tue, 31 Dec 2099 23:59:59 GMT', '--window-size=1920,1080', '--window-position=0,0',
                           f'http://127.0.0.1:{port}/?v={VARIANT}' + os.environ.get('QS', '')],
                          stdout=subprocess.DEVNULL, stderr=open('chrome-stderr.log', 'w'), start_new_session=True)
print('chrome pgid', chrome.pid, flush=True)
try:
    assert ready.wait(40), 'page never reported ready'
    time.sleep(1.5)
    for x, y in ((300, 300), (500, 330), (600, 350)):
        XT.XTestFakeMotionEvent(D, -1, x, y, 0); X11.XFlush(D); time.sleep(0.05)
    if not os.environ.get('NOCLICK'):
        XT.XTestFakeButtonEvent(D, 1, 1, 0); X11.XFlush(D); time.sleep(0.08)
        XT.XTestFakeButtonEvent(D, 1, 0, 0); X11.XFlush(D); time.sleep(0.4)
    seq = [('Shift_L', 1), ('a', 1), ('a', 0), ('Shift_L', 0),
           ('b', 1), ('b', 0),
           ('Shift_L', 1), ('2', 1), ('2', 0), ('Shift_L', 0),
           ('Control_L', 1), ('a', 1), ('a', 0), ('Control_L', 0),
           ('x', 1), ('x', 0),
           ('Return', 1), ('Return', 0)]
    # Pin X input focus to Chrome's top-level window: with no window manager the focus is PointerRoot, and the
    # first key press maps a popup (autofill / IME) that would take every later key.
    X11.XDefaultRootWindow.restype = ctypes.c_ulong; X11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
    X11.XQueryPointer.argtypes = [ctypes.c_void_p, ctypes.c_ulong] + [ctypes.POINTER(ctypes.c_ulong)] * 2 + [ctypes.POINTER(ctypes.c_int)] * 4 + [ctypes.POINTER(ctypes.c_uint)]
    X11.XSetInputFocus.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
    root = X11.XDefaultRootWindow(D); rr = ctypes.c_ulong(); ch = ctypes.c_ulong(); i4 = [ctypes.c_int() for _ in range(4)]; m = ctypes.c_uint()
    X11.XQueryPointer(D, root, ctypes.byref(rr), ctypes.byref(ch), *[ctypes.byref(i) for i in i4], ctypes.byref(m))
    print('focus window', hex(ch.value), flush=True)
    if ch.value and not os.environ.get('NOPIN'):
        X11.XSetInputFocus(D, ch.value, 2, 0); X11.XFlush(D); time.sleep(0.3)
    if os.environ.get('SEQ'):
        seq = [(t.split(':')[0], int(t.split(':')[1])) for t in os.environ['SEQ'].split(',')]
    if os.environ.get('NOCLICK'): pass
    for i, (name, down) in enumerate(seq):
        key(name, down)
        if os.environ.get('XF'): print('after', name, down, 'xfocus', xfocus(), flush=True)
        if os.environ.get('SNAP') and i == 1:
            subprocess.run(['xwininfo', '-root', '-tree'], stdout=open('wintree.txt', 'w'))
            subprocess.run(['xwd', '-root', '-silent', '-out', 'snap.xwd'])
    assert done.wait(20), 'no report'
    open(OUT, 'wb').write(report['body'])
    print('ok', OUT, flush=True)
finally:
    try: os.killpg(chrome.pid, signal.SIGKILL)
    except ProcessLookupError: pass
    chrome.wait()
    srv.shutdown()
