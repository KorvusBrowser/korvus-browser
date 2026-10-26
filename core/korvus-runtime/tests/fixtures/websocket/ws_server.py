#!/usr/bin/env python3
"""A WebSocket echo + INSPECTION server for the WebSocket fixture (`tests/websocket_live.rs`).

It is the SAME server for both engines: stock Chrome 150 is recorded against it
(`record_chrome.sh`), and the korvus test spawns it again. Standard library only.

    python3 -I ws_server.py PORT_FILE LOG_FILE [PAGE_FILE REPORT_FILE]

binds 127.0.0.1:0, writes the port to PORT_FILE, and appends one JSON line per connection to LOG_FILE:
the raw handshake request (status line + every header line, in WIRE order, with its original case), the
route, and every frame the client sent (opcode, FIN, RSV1, MASK bit, payload length, close code/reason).

Routes (the path of the ws:// URL; `?` parameters where noted):

  /echo          echo every message back (same type, one frame, uncompressed)
  /deflate       negotiate permessage-deflate if offered (reply `permessage-deflate; client_max_window_bits=15`),
                 then echo, compressing what it sends and inflating what it gets
  /proto         select the FIRST offered subprotocol, then echo
  /hdr           (selects the first subprotocol like /proto) first message: JSON {names: [...], values: {...}} of the handshake request, then echo
  /cookie        first message: the request's Cookie header ('' when absent), then echo
  /close?code=C&reason=R   send a Close frame right after the handshake, answer the client's Close, close TCP
  /drop          close the TCP connection right after the handshake (no Close frame)
  /status?code=N answer the handshake with status N instead of 101
  /badaccept     answer 101 with a wrong Sec-WebSocket-Accept
  /frag          send "abc"+"def"+"ghi" as one text message in three frames, then [1,2]+[3] binary in two
  /badutf8       send a text frame whose payload is not UTF-8 (FF FE)
  /ping          send Ping "p1"; when the Pong arrives, send text "pong:<payload>"
  /slowclose     answer the client's Close frame but keep the TCP connection open

A plain HTTP request (no `Upgrade: websocket`) is the PAGE side, used when recording Chrome: `GET /` answers
PAGE_FILE with every `__WSPORT__` replaced by the bound port, and `POST /report` writes its body to
REPORT_FILE and stops the server. The probe's page and its sockets therefore share one origin.

Any route: a text message "__report__" is answered with the JSON list of frames received so far on this
connection (the report request itself excluded), so the probe can read the client's framing back.
"""
import asyncio
import base64
import hashlib
import json
import os
import struct
import sys
import zlib
from urllib.parse import parse_qs, urlsplit

GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
LOG = None
PAGE = None
REPORT = None
PORT = 0


def log(rec):
    with open(LOG, "a", encoding="utf-8") as f:
        f.write(json.dumps(rec) + "\n")


def frame(op, payload, fin=True, rsv1=False):
    b0 = (0x80 if fin else 0) | (0x40 if rsv1 else 0) | op
    n = len(payload)
    if n < 126:
        head = bytes([b0, n])
    elif n < 65536:
        head = bytes([b0, 126]) + struct.pack("!H", n)
    else:
        head = bytes([b0, 127]) + struct.pack("!Q", n)
    return head + payload


class Conn:
    def __init__(self, reader, writer):
        self.r, self.w = reader, writer
        self.frames = []
        self.deflate = False
        self.inflater = None
        self.deflater = None

    async def read_frame(self):
        h = await self.r.readexactly(2)
        fin, rsv1, op = bool(h[0] & 0x80), bool(h[0] & 0x40), h[0] & 0x0F
        masked, n = bool(h[1] & 0x80), h[1] & 0x7F
        if n == 126:
            n = struct.unpack("!H", await self.r.readexactly(2))[0]
        elif n == 127:
            n = struct.unpack("!Q", await self.r.readexactly(8))[0]
        key = await self.r.readexactly(4) if masked else b"\0\0\0\0"
        data = bytearray(await self.r.readexactly(n))
        for i in range(n):
            data[i] ^= key[i % 4]
        return fin, rsv1, op, masked, bytes(data)

    def send(self, op, payload, fin=True):
        if self.deflate and op in (1, 2):
            c = self.deflater.compress(payload) + self.deflater.flush(zlib.Z_SYNC_FLUSH)
            if c.endswith(b"\x00\x00\xff\xff"):
                c = c[:-4]
            self.w.write(frame(op, c, fin, rsv1=True))
        else:
            self.w.write(frame(op, payload, fin))

    async def messages(self):
        """Yield (opcode, payload) per complete data message; answer pings; stop at a Close."""
        buf, mop, compressed = b"", None, False
        while True:
            fin, rsv1, op, masked, data = await self.read_frame()
            rec = {"op": op, "fin": fin, "rsv1": rsv1, "masked": masked, "len": len(data)}
            if op == 8:
                if len(data) >= 2:
                    rec["code"] = struct.unpack("!H", data[:2])[0]
                    rec["reason"] = data[2:].decode("utf-8", "replace")
                self.frames.append(rec)
                yield 8, data
                return
            if op == 9:
                self.frames.append(rec)
                self.w.write(frame(10, data))
                continue
            if op == 10:
                self.frames.append(rec)
                yield 10, data
                continue
            if op in (1, 2):
                mop, buf, compressed = op, data, rsv1
            else:
                buf += data
            if not fin:
                self.frames.append(rec)
                continue
            if compressed:
                buf = self.inflater.decompress(buf + b"\x00\x00\xff\xff")
            if mop == 1 and buf == b"__report__":
                self.send(1, json.dumps(self.frames).encode())
                await self.w.drain()
                continue
            self.frames.append(rec)
            yield mop, buf


async def handle(reader, writer):
    raw = b""
    try:
        while b"\r\n\r\n" not in raw:
            chunk = await reader.read(4096)
            if not chunk:
                writer.close()
                return
            raw += chunk
    except Exception:
        writer.close()
        return
    head = raw.split(b"\r\n\r\n", 1)[0].decode("latin-1")
    lines = head.split("\r\n")
    request_line, header_lines = lines[0], lines[1:]
    headers = []
    for l in header_lines:
        k, _, v = l.partition(":")
        headers.append((k, v.strip()))
    hmap = {}
    for k, v in headers:
        hmap.setdefault(k.lower(), v)
    target = request_line.split(" ")[1] if " " in request_line else "/"
    if hmap.get("upgrade", "").lower() != "websocket":
        await serve_http(request_line, target, hmap, raw.split(b"\r\n\r\n", 1)[1], reader, writer)
        return
    u = urlsplit(target)
    path, q = u.path, parse_qs(u.query)
    rec = {"route": path, "request_line": request_line, "headers": headers, "frames": []}
    c = Conn(reader, writer)

    if path == "/status":
        code = int(q.get("code", ["404"])[0])
        writer.write(f"HTTP/1.1 {code} Nope\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".encode())
        await writer.drain()
        writer.close()
        log(rec)
        return

    key = hmap.get("sec-websocket-key", "").encode()
    accept = base64.b64encode(hashlib.sha1(key + GUID).digest()).decode()
    if path == "/badaccept":
        accept = base64.b64encode(b"0" * 20).decode()
    resp = ["HTTP/1.1 101 Switching Protocols", "Upgrade: websocket", "Connection: Upgrade", "Sec-WebSocket-Accept: " + accept]
    if path in ("/proto", "/hdr") and hmap.get("sec-websocket-protocol"):
        resp.append("Sec-WebSocket-Protocol: " + hmap["sec-websocket-protocol"].split(",")[0].strip())
    if path == "/deflate" and "permessage-deflate" in hmap.get("sec-websocket-extensions", ""):
        resp.append("Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits=15")
        c.deflate = True
        c.inflater = zlib.decompressobj(-15)
        c.deflater = zlib.compressobj(zlib.Z_DEFAULT_COMPRESSION, zlib.DEFLATED, -15)
    writer.write(("\r\n".join(resp) + "\r\n\r\n").encode())
    await writer.drain()

    try:
        if path == "/drop":
            writer.close()
            return
        if path == "/hdr":
            info = {"request_line": request_line, "names": [k for k, _ in headers], "values": {k: v for k, v in headers}}
            c.send(1, json.dumps(info).encode())
        if path == "/cookie":
            c.send(1, hmap.get("cookie", "").encode())
        if path == "/close":
            code = int(q.get("code", ["1000"])[0])
            reason = q.get("reason", [""])[0].encode()
            c.send(8, struct.pack("!H", code) + reason)
        if path == "/frag":
            c.w.write(frame(1, b"abc", fin=False) + frame(0, b"def", fin=False) + frame(0, b"ghi", fin=True))
            c.w.write(frame(2, b"\x01\x02", fin=False) + frame(0, b"\x03", fin=True))
        if path == "/badutf8":
            c.w.write(frame(1, b"\xff\xfe"))
        if path == "/ping":
            c.w.write(frame(9, b"p1"))
        await writer.drain()
        async for op, data in c.messages():
            if op == 8:
                if path == "/close":
                    # The client's answer to our Close: the closing handshake is complete; close TCP.
                    break
                # Echo the client's Close payload (RFC 6455 5.5.1), then close TCP -- unless /slowclose.
                c.w.write(frame(8, data))
                await writer.drain()
                if path == "/slowclose":
                    # Logged NOW: the client is expected to drop TCP itself after its 2 s wait, and the
                    # record must not depend on how long this server outlives the probe.
                    rec["frames"] = list(c.frames)
                    log(rec)
                    rec["logged"] = True
                    await asyncio.sleep(5)
                break
            if op == 10:
                if path == "/ping":
                    c.send(1, b"pong:" + data)
                    await writer.drain()
                continue
            c.send(op, data)
            await writer.drain()
    except (asyncio.IncompleteReadError, ConnectionError):
        pass
    finally:
        if not rec.get("logged"):
            rec["frames"] = c.frames
            log(rec)
        try:
            writer.close()
        except Exception:
            pass


async def serve_http(request_line, target, hmap, body, reader, writer):
    """The page side: the probe page at `/`, its answer at `POST /report`."""
    method = request_line.split(" ")[0]
    if method == "POST" and target == "/report":
        n = int(hmap.get("content-length", "0"))
        while len(body) < n:
            chunk = await reader.read(n - len(body))
            if not chunk:
                break
            body += chunk
        writer.write(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        await writer.drain()
        writer.close()
        if REPORT:
            with open(REPORT, "wb") as f:
                f.write(body)
            asyncio.get_running_loop().call_later(0.2, lambda: os._exit(0))
        return
    if method == "GET" and target == "/" and PAGE:
        b = open(PAGE, encoding="utf-8").read().replace("__WSPORT__", str(PORT)).encode()
        writer.write(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: " + str(len(b)).encode() + b"\r\nConnection: close\r\n\r\n" + b)
    else:
        writer.write(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    await writer.drain()
    writer.close()


async def main():
    global LOG, PAGE, REPORT, PORT
    port_file, LOG = sys.argv[1], sys.argv[2]
    if len(sys.argv) > 4:
        PAGE, REPORT = sys.argv[3], sys.argv[4]
    # `WS_FIXTURE_HOST` picks the loopback address (default 127.0.0.1, what Chrome was recorded on). The
    # korvus test binds a loopback address of its own so no other test's cookies share its host:
    # cookies are scoped by host, not by port, and the transport's jar is process-wide.
    srv = await asyncio.start_server(handle, os.environ.get("WS_FIXTURE_HOST", "127.0.0.1"), 0)
    port = PORT = srv.sockets[0].getsockname()[1]
    tmp = port_file + ".tmp"
    with open(tmp, "w") as f:
        f.write(str(port))
    os.replace(tmp, port_file)
    async with srv:
        await srv.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
