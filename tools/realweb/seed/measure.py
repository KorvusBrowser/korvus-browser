#!/usr/bin/env python3
"""Measure every seed fixture in stock Chrome (the expected value) and in Korvus, and write manifest.json.
usage: measure.py [--korvus BIN] [--only ID,...]   (Chrome runs headless-new; audio needs real time, which it gets)"""
import json, os, subprocess, sys, time, signal
HERE = os.path.dirname(os.path.abspath(__file__)); REPO = os.path.abspath(os.path.join(HERE, '..', '..', '..'))
KORVUS = os.path.join(REPO, 'target', 'release', 'korvus')
if '--korvus' in sys.argv: KORVUS = sys.argv[sys.argv.index('--korvus') + 1]
ONLY = set(sys.argv[sys.argv.index('--only') + 1].split(',')) if '--only' in sys.argv else None
SPECS = json.load(open(os.path.join(HERE, 'specs.json')))
def run(spec):
    srv = subprocess.Popen([sys.executable, os.path.join(HERE, 'serve.py'), os.path.join(HERE, spec['dir'])] + [str(p) for p in spec['ports']],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    time.sleep(0.6); out = {}
    try:
        env = dict(os.environ, CHROME_HEADLESS='1')
        c = subprocess.run([sys.executable, os.path.join(HERE, 'chrome_eval.py'), spec['url'], spec['expr'], str(spec['wait_ms'])], capture_output=True, text=True, timeout=90, env=env)
        out['chrome'] = c.stdout.strip().splitlines()[-1] if c.stdout.strip() else 'ERR ' + c.stderr.strip()[-200:]
        k = subprocess.run([KORVUS, 'fetch', spec['url'], '--wait', str(spec['wait_ms'] // 1000 + 1), '--eval', spec['expr']], capture_output=True, text=True, timeout=120)
        out['korvus'] = k.stdout.strip().splitlines()[-1] if k.stdout.strip() else 'ERR ' + k.stderr.strip()[-200:]
    finally:
        os.killpg(srv.pid, signal.SIGKILL); srv.wait()
    return out
man_p = os.path.join(HERE, 'manifest.json'); man = json.load(open(man_p)) if os.path.exists(man_p) else {}
for spec in SPECS:
    if ONLY and spec['id'] not in ONLY: continue
    r = run(spec); m = dict(spec); m['chrome_expected'] = r['chrome']; m['korvus_observed'] = r['korvus']; m['measured'] = time.strftime('%Y-%m-%d')
    man[spec['id']] = m
    print(f"{spec['id']:26} {'EQUAL' if r['chrome'] == r['korvus'] else 'DIFF '}  chrome={r['chrome'][:70]!r}  korvus={r['korvus'][:70]!r}", flush=True)
json.dump(man, open(man_p, 'w'), ensure_ascii=False, indent=1)
