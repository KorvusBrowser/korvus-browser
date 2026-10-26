#!/bin/bash
# record.sh PROBE OUT: one recording of PROBE (schemes.js, cache.js, order.js) in stock Chrome 150 (/usr/bin/google-chrome
# 150.0.7871.46), headed under Xvfb :99 (--ozone-platform=x11; set XAUTHORITY to the Xvfb's), NO DevTools attached; its
# rows to OUT, every ":<port>" written ":PORT" (the test normalises korvus's rows the same way). The fixtures
# `<probe>.chrome-150.json` are three such runs alike.
D=$(cd "$(dirname "$0")" && pwd)
PROBE=$1; OUT=$2
PORT=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')
: > "$OUT.raw"
python3 "$D/srv.py" $PORT "$PROBE" "$OUT.raw" & SRV=$!
sleep 0.4
PROF=$(mktemp -d)
DISPLAY=:99 setsid /usr/bin/google-chrome --ozone-platform=x11 --user-data-dir=$PROF --no-first-run --no-default-browser-check --window-size=1920,1080 "http://127.0.0.1:$PORT/" >/dev/null 2>&1 &
CPID=$!
for i in $(seq 1 240); do [ -s "$OUT.raw" ] && break; sleep 0.25; done
kill -9 -$CPID 2>/dev/null; kill $SRV 2>/dev/null; wait 2>/dev/null
python3 -c '
import json, re, sys
t = open(sys.argv[1]).read()
if not t: print("NO ANSWER", file=sys.stderr); sys.exit(1)
t = re.sub(r":(\d{2,5})(?![0-9A-Za-z_])", ":PORT", t)
json.dump(json.loads(t), open(sys.argv[2], "w"), indent=0)
' "$OUT.raw" "$OUT" && rm -f "$OUT.raw" && echo "$OUT"
