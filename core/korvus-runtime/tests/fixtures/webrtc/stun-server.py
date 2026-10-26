# A STUN server for fixtures: every Binding Request (RFC 5389) is answered with XOR-MAPPED-ADDRESS
# 203.0.113.9:<the request's source port>, and logged (source port, transaction id, attribute bytes).
import socket, struct, sys, time
port = int(sys.argv[1]); log = open(sys.argv[2], 'a', buffering=1)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.bind(('127.0.0.1', port))
MAGIC = 0x2112A442
while True:
    data, addr = s.recvfrom(2048)
    if len(data) < 20: continue
    t, ln, magic = struct.unpack('!HHI', data[:8]); tid = data[8:20]
    log.write(f"{time.time():.3f} from {addr[1]} type {t:#06x} len {ln} tid {tid!r} attrs {data[20:].hex()}\n")
    if t != 0x0001 or magic != MAGIC: continue
    ip = bytes([203, 0, 113, 9]); xport = addr[1] ^ (MAGIC >> 16)
    xip = bytes(a ^ b for a, b in zip(ip, struct.pack('!I', MAGIC)))
    attr = struct.pack('!HHBBH', 0x0020, 8, 0, 1, xport) + xip
    s.sendto(struct.pack('!HHI', 0x0101, len(attr), MAGIC) + tid + attr, addr)
