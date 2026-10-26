#!/usr/bin/env python3
"""cdp_vk_check.py OUT: does CDP Input.dispatchKeyEvent WITHOUT windowsVirtualKeyCode give keyCode 0 (input_handler.cc)?
Raw CDP (no Playwright), stock Chrome 150 on $DISPLAY. Also: modifiers=8 -> shiftKey; type 'keyPress' -> error."""
import json, os, signal, socket, subprocess, sys, tempfile, time, urllib.request
import websocket
OUT = sys.argv[1]
s = socket.socket(); s.bind(('127.0.0.1', 0)); port = s.getsockname()[1]; s.close()
PAGE = ("data:text/html,<input id=t><script>window.R=[];['keydown','keypress','keyup','beforeinput','input'].forEach(k=>"
        "addEventListener(k,e=>R.push([e.type,e.key,e.code,e.keyCode,e.which,e.charCode,e.shiftKey,e.location,e.repeat,e.data,t.value]),true));t.focus()</script>")
prof = tempfile.mkdtemp(prefix='c7cdp-', dir='/tmp/claude-1000')
p = subprocess.Popen(['/usr/bin/google-chrome', '--ozone-platform=x11', f'--remote-debugging-port={port}', f'--user-data-dir={prof}',
                      '--no-first-run', '--no-default-browser-check', '--password-store=basic',
                      '--simulate-outdated-no-au=Tue, 31 Dec 2099 23:59:59 GMT', 'about:blank'],
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
print('chrome pgid', p.pid, flush=True)
try:
    for _ in range(100):
        try: socket.create_connection(('127.0.0.1', port), 0.2).close(); break
        except OSError: time.sleep(0.1)
    time.sleep(1)
    pg = [t for t in json.loads(urllib.request.urlopen(f'http://127.0.0.1:{port}/json/list').read()) if t['type'] == 'page'][0]
    ws = websocket.create_connection(pg['webSocketDebuggerUrl'], suppress_origin=True)
    n = [0]
    def call(m, params):
        n[0] += 1; ws.send(json.dumps({'id': n[0], 'method': m, 'params': params}))
        while True:
            r = json.loads(ws.recv())
            if r.get('id') == n[0]: return r
    call('Page.navigate', {'url': PAGE}); time.sleep(1.5)
    res = {}
    call('Input.dispatchKeyEvent', {'type': 'keyDown', 'key': 'a', 'code': 'KeyA', 'text': 'a'})
    call('Input.dispatchKeyEvent', {'type': 'keyUp', 'key': 'a', 'code': 'KeyA'})
    call('Input.dispatchKeyEvent', {'type': 'keyDown', 'key': 'Shift', 'code': 'ShiftLeft', 'windowsVirtualKeyCode': 16, 'modifiers': 8, 'location': 1})
    call('Input.dispatchKeyEvent', {'type': 'keyDown', 'key': 'Backspace', 'code': 'Backspace'})
    call('Input.dispatchKeyEvent', {'type': 'keyDown', 'key': 'b', 'code': 'KeyB'})
    res['keyPress'] = call('Input.dispatchKeyEvent', {'type': 'keyPress', 'key': 'a'})
    res['text4'] = call('Input.dispatchKeyEvent', {'type': 'keyDown', 'key': 'a', 'text': 'abcd'})
    res['rows'] = call('Runtime.evaluate', {'expression': 'JSON.stringify(R)', 'returnByValue': True})['result']['result']['value']
    json.dump(res, open(OUT, 'w'), indent=1)
    print(json.dumps(res, indent=1))
finally:
    os.killpg(p.pid, signal.SIGKILL); p.wait()
