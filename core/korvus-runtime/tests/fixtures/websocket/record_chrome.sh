#!/bin/bash
# record_chrome.sh OUT_DIR RUN_TAG -- record stock Chrome 150 running ws-probe.js against ws_server.py.
#
# Headed, under Xvfb :99 with --ozone-platform=x11 (the login is Wayland), a fresh profile, NO DevTools
# client attached (a CDP client changes answers). Run it through abkit/live_slot.sh. The page and the
# sockets share one origin: ws_server.py serves the page at `/` and takes the probe's answer at
# `POST /report`. Writes OUT_DIR/chrome-RUN_TAG.json (the probe rows) and OUT_DIR/chrome-RUN_TAG.log
# (the server's handshake + frame log, one JSON line per connection).
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=$1; TAG=$2
mkdir -p "$OUT"
WORK=$(mktemp -d /tmp/claude-1000/wsrec-XXXX)
python3 - "$HERE/ws-probe.js" "$WORK/page.html" <<'EOF'
import sys
probe = open(sys.argv[1], encoding="utf-8").read()
page = ("<!doctype html><html><head><meta charset=utf-8></head><body><script>\n"
        "var __r;\ntry { __r = " + probe + "; } catch (e) { __r = [['FATAL-OUTER', String(e)]]; }\n"
        "Promise.resolve(__r).then(function (r) { window.__report = JSON.stringify(r); "
        "fetch('/report', {method: 'POST', body: window.__report}); });\n</script></body></html>")
open(sys.argv[2], "w", encoding="utf-8").write(page)
EOF
: > "$WORK/server.log"
python3 -I "$HERE/ws_server.py" "$WORK/port" "$WORK/server.log" "$WORK/page.html" "$WORK/report.json" & SRV=$!
for i in $(seq 1 50); do [ -s "$WORK/port" ] && break; sleep 0.1; done
PORT=$(cat "$WORK/port")
PROF=$(mktemp -d /tmp/claude-1000/nocdp-XXXX)
DISPLAY=:99 setsid /usr/bin/google-chrome --ozone-platform=x11 --user-data-dir="$PROF" --no-first-run \
  --no-default-browser-check --window-size=1920,1080 "http://127.0.0.1:$PORT/" >/dev/null 2>&1 &
CPID=$!
for i in $(seq 1 480); do [ -s "$WORK/report.json" ] && break; sleep 0.25; done
kill -9 -$CPID 2>/dev/null; kill $SRV 2>/dev/null; wait 2>/dev/null
[ -s "$WORK/report.json" ] || { echo "NO REPORT (work dir $WORK)"; exit 1; }
python3 - "$WORK/report.json" "$OUT/chrome-$TAG.json" "$PORT" <<'EOF'
import json, sys
t = open(sys.argv[1], encoding="utf-8").read().replace(":" + sys.argv[3], ":PORT")
json.dump(json.loads(t), open(sys.argv[2], "w"), indent=0)
EOF
sed "s/:$PORT/:PORT/g" "$WORK/server.log" > "$OUT/chrome-$TAG.log"
echo "chrome -> $OUT/chrome-$TAG.json ($(python3 -c 'import json,sys;print(len(json.load(open(sys.argv[1]))))' "$OUT/chrome-$TAG.json") rows)"
