# A TURN server for fixtures (RFC 5766 Allocate under the long-term credential mechanism, RFC 5389 sec. 10.2): an
# unauthenticated Allocate is answered 401 with REALM "korvus" and a NONCE; an authenticated one (username "u",
# password "p") with XOR-RELAYED-ADDRESS 198.51.100.7:<the request's source port + 1>, XOR-MAPPED-ADDRESS
# 203.0.113.9:<its source port> and LIFETIME 600; a Refresh, a CreatePermission and a Binding Request are answered
# too. Every request is logged (time, source port, type, attribute types). No relaying is done.
import hashlib, hmac, os, socket, struct, sys, time, zlib
port = int(sys.argv[1]); log = open(sys.argv[2], 'a', buffering=1)
MAGIC = 0x2112A442
REALM, USER, PASSWORD = b'korvus', b'u', b'p'
KEY = hashlib.md5(USER + b':' + REALM + b':' + PASSWORD).digest()
NONCE = os.urandom(8).hex().encode()
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.bind(('127.0.0.1', port))

def attrs_of(body):
    out, i = [], 0
    while i + 4 <= len(body):
        t, n = struct.unpack('!HH', body[i:i + 4])
        out.append((t, body[i + 4:i + 4 + n]))
        i += 4 + n + (-n % 4)
    return out

def attr(t, v):
    return struct.pack('!HH', t, len(v)) + v + b'\0' * (-len(v) % 4)

def xaddr(t, ip, p):
    xp = p ^ (MAGIC >> 16)
    xip = bytes(a ^ b for a, b in zip(bytes(ip), struct.pack('!I', MAGIC)))
    return attr(t, struct.pack('!BBH', 0, 1, xp) + xip)

def message(t, tid, body, integrity):
    if integrity:
        # MESSAGE-INTEGRITY over the message as it stands, its length counting the 24-byte attribute
        head = struct.pack('!HHI', t, len(body) + 24, MAGIC) + tid
        mac = hmac.new(KEY, head + body, hashlib.sha1).digest()
        body += attr(0x0008, mac)
    # FINGERPRINT: CRC-32 of the message before it, XOR 0x5354554e, its length counting the 8-byte attribute
    head = struct.pack('!HHI', t, len(body) + 8, MAGIC) + tid
    crc = (zlib.crc32(head + body) & 0xffffffff) ^ 0x5354554e
    body += attr(0x8028, struct.pack('!I', crc))
    return struct.pack('!HHI', t, len(body), MAGIC) + tid + body

def error(t, tid, code, reason, extra=b''):
    v = struct.pack('!HBB', 0, code // 100, code % 100) + reason
    return message(t | 0x0110, tid, attr(0x0009, v) + extra, False)

while True:
    data, addr = s.recvfrom(4096)
    if len(data) < 20: continue
    t, ln, magic = struct.unpack('!HHI', data[:8]); tid = data[8:20]
    if magic != MAGIC: continue
    a = attrs_of(data[20:20 + ln])
    names = dict(a)
    log.write(f"{time.time():.3f} from {addr[1]} type {t:#06x} attrs {[hex(x) for x, _ in a]}\n")
    mapped = xaddr(0x0020, [203, 0, 113, 9], addr[1])
    if t == 0x0001:
        s.sendto(message(0x0101, tid, mapped, False), addr)
        continue
    if t not in (0x0003, 0x0004, 0x0008):
        continue
    if 0x0008 not in names or names.get(0x0006) != USER:
        s.sendto(error(t, tid, 401, b'Unauthorized', attr(0x0014, REALM) + attr(0x0015, NONCE)), addr)
        continue
    if t == 0x0003:
        body = xaddr(0x0016, [198, 51, 100, 7], (addr[1] + 1) & 0xffff) + mapped + attr(0x000D, struct.pack('!I', 600))
    elif t == 0x0004:
        lifetime = names.get(0x000D, struct.pack('!I', 600))
        body = attr(0x000D, lifetime)
    else:
        body = b''
    s.sendto(message(t | 0x0100, tid, body, True), addr)
