"""h3rec.py: record what a client puts on an HTTP/3 (QUIC v1) wire, and over TCP+TLS on the same port.

One port number, two servers: a TLS (HTTP/1.1) server on TCP that answers every request with `Alt-Svc: h3=":PORT"`,
and an aioquic HTTP/3 server on UDP. Every UDP datagram from the client is kept raw; the client's Initial packets are
decrypted here (RFC 9001 5.2 initial keys) so the ClientHello, its extensions, the QUIC transport parameters and the
frame layout of the first flight are read off the bytes, not off a library's parse. Every byte the client writes on
every QUIC stream is kept too, so the HTTP/3 control stream (SETTINGS, PRIORITY_UPDATE, GREASE) and each request's
HEADERS frame are parsed here as well, beside aioquic's decoded header lists.

  python3 h3rec.py PORT OUT.json [--secs N]

The page `/` loads a script, a stylesheet, an image and a fetch(); after `load` it waits and fetches `/late` (by then
the Alt-Svc is known), then beacons `/done`. The recording ends at `/done` or after --secs.
"""
import asyncio, json, os, ssl, struct, sys, time

from aioquic.asyncio import serve
from aioquic.asyncio.protocol import QuicConnectionProtocol
from aioquic.asyncio.server import QuicServer
from aioquic.h3.connection import H3_ALPN, H3Connection
from aioquic.h3.events import HeadersReceived, DataReceived
from aioquic.quic.configuration import QuicConfiguration
from aioquic.quic.events import StreamDataReceived, HandshakeCompleted, ProtocolNegotiated
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.primitives import hashes, hmac

HERE = os.path.dirname(os.path.abspath(__file__))
PORT = int(sys.argv[1])
OUT = sys.argv[2]
SECS = float(sys.argv[sys.argv.index('--secs') + 1]) if '--secs' in sys.argv else 25.0
T0 = time.monotonic()

REC = {'datagrams': [], 'streams': {}, 'requests': [], 'events': []}
DONE = asyncio.Event()


def ms():
    return round((time.monotonic() - T0) * 1000, 1)


# ---- RFC 9001 initial protection -------------------------------------------------------------------------------
V1_SALT = bytes.fromhex('38762cf7f55934b34d179ae6a4c80cadccbb7f0a')


def hkdf_extract(salt, ikm):
    h = hmac.HMAC(salt, hashes.SHA256()); h.update(ikm); return h.finalize()


def hkdf_expand_label(secret, label, length):
    full = b'tls13 ' + label.encode()
    info = struct.pack('>H', length) + bytes([len(full)]) + full + b'\x00'
    out, t, i = b'', b'', 1
    while len(out) < length:
        h = hmac.HMAC(secret, hashes.SHA256()); h.update(t + info + bytes([i])); t = h.finalize(); out += t; i += 1
    return out[:length]


def initial_keys(dcid):
    s = hkdf_extract(V1_SALT, dcid)
    c = hkdf_expand_label(s, 'client in', 32)
    return hkdf_expand_label(c, 'quic key', 16), hkdf_expand_label(c, 'quic iv', 12), hkdf_expand_label(c, 'quic hp', 16)


def varint(b, i):
    first = b[i]; n = 1 << (first >> 6); v = first & 0x3f
    for k in range(1, n):
        v = (v << 8) | b[i + k]
    return v, i + n


FIRST_DCID = {}  # client addr -> original destination connection id (the Initial keys' input)


def parse_long(b, i, addr):
    """One long-header packet at b[i:]; returns (record, next offset)."""
    first = b[i]; ver = struct.unpack('>I', b[i + 1:i + 5])[0]; j = i + 5
    dl = b[j]; dcid = b[j + 1:j + 1 + dl]; j += 1 + dl
    sl = b[j]; scid = b[j + 1:j + 1 + sl]; j += 1 + sl
    ptype = (first >> 4) & 3
    rec = {'type': ['initial', '0rtt', 'handshake', 'retry'][ptype], 'version': '%08x' % ver, 'dcid_len': dl, 'scid_len': sl,
           'fixed_bit': (first >> 6) & 1}
    if ptype == 0:
        tl, j = varint(b, j); rec['token_len'] = tl; j += tl
    if ptype == 3:
        return rec, len(b)
    ln, j = varint(b, j); rec['length'] = ln
    end = j + ln
    if ptype == 0:
        key, iv, hp = initial_keys(FIRST_DCID.setdefault(addr, dcid))
        pn_off = j
        sample = b[pn_off + 4:pn_off + 20]
        enc = Cipher(algorithms.AES(hp), modes.ECB()).encryptor()
        mask = enc.update(sample) + enc.finalize()
        f = first ^ (mask[0] & 0x0f)
        pnl = (f & 3) + 1
        pn_bytes = bytes(b[pn_off + k] ^ mask[1 + k] for k in range(pnl))
        pn = int.from_bytes(pn_bytes, 'big')
        hdr = bytes([f]) + b[i + 1:pn_off] + pn_bytes
        nonce = (int.from_bytes(iv, 'big') ^ pn).to_bytes(12, 'big')
        try:
            plain = AESGCM(key).decrypt(nonce, b[pn_off + pnl:end], hdr)
        except Exception as e:
            rec['decrypt'] = 'failed: %r' % e
            return rec, end
        rec['pn'] = pn; rec['pn_len'] = pnl; rec['reserved_bits'] = (f >> 2) & 3
        rec['frames'] = parse_frames(plain)
    return rec, end


def parse_frames(p):
    out, i = [], 0
    while i < len(p):
        t, i2 = varint(p, i)
        if t == 0:
            n = 0
            while i < len(p) and p[i] == 0:
                i += 1; n += 1
            out.append({'f': 'PADDING', 'n': n}); continue
        i = i2
        if t == 1:
            out.append({'f': 'PING'})
        elif t == 6:
            off, i = varint(p, i); ln, i = varint(p, i)
            out.append({'f': 'CRYPTO', 'off': off, 'len': ln, '_data': p[i:i + ln].hex()}); i += ln
        elif t in (2, 3):
            la, i = varint(p, i); dl, i = varint(p, i); rc, i = varint(p, i); fr, i = varint(p, i)
            for _ in range(rc):
                _, i = varint(p, i); _, i = varint(p, i)
            if t == 3:
                for _ in range(3):
                    _, i = varint(p, i)
            out.append({'f': 'ACK', 'largest': la})
        elif t == 0x1c or t == 0x1d:
            out.append({'f': 'CONNECTION_CLOSE', 'raw': p[i:].hex()}); break
        else:
            out.append({'f': 'type 0x%x' % t, 'rest': p[i:].hex()}); break
    return out


def is_grease(v):
    return (v & 0x0f0f) == 0x0a0a and (v >> 8) == (v & 0xff)


EXT_NAMES = {0: 'server_name', 5: 'status_request', 10: 'supported_groups', 11: 'ec_point_formats', 13: 'signature_algorithms',
             16: 'alpn', 18: 'signed_certificate_timestamp', 21: 'padding', 23: 'extended_master_secret', 27: 'compress_certificate',
             35: 'session_ticket', 41: 'pre_shared_key', 42: 'early_data', 43: 'supported_versions', 45: 'psk_key_exchange_modes',
             51: 'key_share', 57: 'quic_transport_parameters', 17513: 'application_settings_old', 17613: 'application_settings',
             65037: 'encrypted_client_hello', 65281: 'renegotiation_info'}

TP_NAMES = {0x00: 'original_destination_connection_id', 0x01: 'max_idle_timeout', 0x02: 'stateless_reset_token',
            0x03: 'max_udp_payload_size', 0x04: 'initial_max_data', 0x05: 'initial_max_stream_data_bidi_local',
            0x06: 'initial_max_stream_data_bidi_remote', 0x07: 'initial_max_stream_data_uni', 0x08: 'initial_max_streams_bidi',
            0x09: 'initial_max_streams_uni', 0x0a: 'ack_delay_exponent', 0x0b: 'max_ack_delay', 0x0c: 'disable_active_migration',
            0x0d: 'preferred_address', 0x0e: 'active_connection_id_limit', 0x0f: 'initial_source_connection_id',
            0x10: 'retry_source_connection_id', 0x11: 'version_information', 0x20: 'max_datagram_frame_size',
            0x2ab2: 'grease_quic_bit', 0xff04de1b: 'min_ack_delay', 0x3127: 'initial_round_trip_time',
            0x3128: 'google_connection_options', 0x3129: 'user_agent_id', 0x4752: 'google_version',
            0x71: 'reliable_stream_reset', 0x173e: 'discard'}

VARINT_TPS = {0x01, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0e, 0x20, 0xff04de1b, 0x3127}


def parse_tp(d):
    out, i = [], 0
    while i < len(d):
        pid, i = varint(d, i); ln, i = varint(d, i); val = d[i:i + ln]; i += ln
        row = {'id': '0x%x' % pid, 'name': TP_NAMES.get(pid, 'GREASE' if (pid - 27) % 31 == 0 else '?'), 'len': ln}
        if pid in VARINT_TPS and ln:
            row['value'] = varint(val, 0)[0]
        elif row['name'] == 'GREASE':
            row['len_class'] = ln
        elif pid == 0x11:
            row['value'] = [val[k:k + 4].hex() for k in range(0, ln, 4)]
        elif pid == 0x3128:
            row['value'] = [val[k:k + 4].decode('latin1') for k in range(0, ln, 4)]
        elif pid == 0x3129:
            row['value'] = val.decode('latin1')
        elif pid == 0x4752:
            row['value'] = val.hex()
        elif pid == 0x0f:
            row['value'] = 'len %d' % ln
        else:
            row['value'] = val.hex()
        out.append(row)
    return out


def parse_client_hello(ch):
    assert ch[0] == 1, ch[:4].hex()
    i = 4; r = {'legacy_version': ch[i:i + 2].hex()}; i += 2 + 32
    sl = ch[i]; r['session_id_len'] = sl; i += 1 + sl
    cl = struct.unpack('>H', ch[i:i + 2])[0]; i += 2
    r['ciphers'] = ['GREASE' if is_grease(c) else '%04x' % c for c in struct.unpack('>%dH' % (cl // 2), ch[i:i + cl])]; i += cl
    ml = ch[i]; r['compression'] = ch[i + 1:i + 1 + ml].hex(); i += 1 + ml
    el = struct.unpack('>H', ch[i:i + 2])[0]; i += 2; end = i + el
    exts = []
    while i < end:
        t, ln = struct.unpack('>HH', ch[i:i + 4]); d = ch[i + 4:i + 4 + ln]; i += 4 + ln
        e = {'type': 'GREASE' if is_grease(t) else t, 'name': 'GREASE' if is_grease(t) else EXT_NAMES.get(t, '?'), 'len': ln}
        if t == 16:
            names, k = [], 2
            while k < len(d):
                n = d[k]; names.append(d[k + 1:k + 1 + n].decode()); k += 1 + n
            e['value'] = names
        elif t in (17513, 17613):
            names, k = [], 2
            while k < len(d):
                n = d[k]; names.append(d[k + 1:k + 1 + n].decode()); k += 1 + n
            e['value'] = names
        elif t == 10:
            e['value'] = ['GREASE' if is_grease(g) else '%04x' % g for g in struct.unpack('>%dH' % ((ln - 2) // 2), d[2:])]
        elif t == 13:
            e['value'] = ['%04x' % g for g in struct.unpack('>%dH' % ((ln - 2) // 2), d[2:])]
        elif t == 43:
            e['value'] = ['GREASE' if is_grease(g) else '%04x' % g for g in struct.unpack('>%dH' % (d[0] // 2), d[1:])]
        elif t == 51:
            ks, k = [], 2
            while k < len(d):
                g, n = struct.unpack('>HH', d[k:k + 4]); ks.append(['GREASE' if is_grease(g) else '%04x' % g, n]); k += 4 + n
            e['value'] = ks
        elif t == 27:
            e['value'] = ['%04x' % g for g in struct.unpack('>%dH' % (d[0] // 2), d[1:])]
        elif t == 45 or t == 11:
            e['value'] = d[1:].hex()
        elif t == 0:
            e['value'] = d[5:].decode()
        elif t == 57:
            e['value'] = parse_tp(d)
        elif t == 65037:
            e['value'] = {'len': ln, 'kdf_aead': d[1:5].hex() if ln > 5 else ''}
        elif t == 5:
            e['value'] = d.hex()
        else:
            e['value'] = d.hex() if ln <= 8 else 'len %d' % ln
        exts.append(e)
    r['extensions'] = exts
    return r


def summarize_initial(port=None):
    """Reassemble one client connection's Initial CRYPTO stream (by source port) and parse the ClientHello."""
    data = {}
    for d in REC['datagrams']:
        if port is not None and d.get('from') != port:
            continue
        for p in d.get('packets', []):
            for f in p.get('frames', []):
                if f['f'] == 'CRYPTO':
                    data[f['off']] = bytes.fromhex(f['_data'])
    buf, off = b'', 0
    while off in data:
        buf += data[off]; off += len(data[off])
    if len(buf) < 4:
        return None
    ln = int.from_bytes(buf[1:4], 'big')
    if len(buf) < 4 + ln:
        return {'incomplete': len(buf)}
    return parse_client_hello(buf[:4 + ln])


# ---- the QUIC side ---------------------------------------------------------------------------------------------
class RecServer(QuicServer):
    def datagram_received(self, data, addr):
        if len(REC['datagrams']) < 400:
            d = {'t': ms(), 'len': len(data), 'from': addr[1], 'hex': data.hex() if len(REC['datagrams']) < 64 else ''}
            if data[0] & 0x80:
                pk, i = [], 0
                try:
                    while i < len(data):
                        if not data[i] & 0x80:
                            pk.append({'type': 'short', 'len': len(data) - i}); break
                        r, i = parse_long(data, i, addr)
                        pk.append(r)
                except Exception as e:
                    pk.append({'error': repr(e)})
                d['packets'] = pk
                trail = len(data) - i
                if trail > 0:
                    d['trailing'] = trail
            else:
                d['short'] = True
            REC['datagrams'].append(d)
        _ORIG_DGRAM(self, data, addr)


BODIES = {}
CONN_SEQ = 0


def page(scheme):
    return ('<!doctype html><html><head><link rel=stylesheet href="/s.css"><script src="/a.js"></script></head>'
            '<body><img src="/i.png"><script>'
            'fetch("/f",{method:"GET"}).then(r=>r.text());'
            'addEventListener("load",()=>setTimeout(()=>{fetch("/late").then(r=>r.text()).then(()=>'
            'fetch("/late-post",{method:"POST",body:"x=1"})).then(()=>{let i=new Image();i.src="/late.png";'
            'setTimeout(()=>fetch("/done"),800)})},1500));'
            '</script></body></html>').encode()


PNG = bytes.fromhex('89504e470d0a1a0a0000000d4948445200000001000000010806000000'
                    '1f15c4890000000d49444154789c6360000002000105fe02fea70000000049454e44ae426082')


def body_for(path):
    p = path.split('?')[0]
    if p == '/':
        return 'text/html', page('')
    if p.endswith('.js'):
        return 'text/javascript', b'window.a=1;'
    if p.endswith('.css'):
        return 'text/css', b'body{color:#333}'
    if p.endswith('.png'):
        return 'image/png', PNG
    return 'text/plain', b'ok'


class H3Rec(QuicConnectionProtocol):
    def __init__(self, *a, **k):
        super().__init__(*a, **k)
        self._h3 = None
        global CONN_SEQ
        CONN_SEQ += 1
        self._cid = CONN_SEQ

    def quic_event_received(self, ev):
        if isinstance(ev, ProtocolNegotiated):
            self._h3 = H3Connection(self._quic)
            REC['events'].append({'t': ms(), 'conn': self._cid, 'ev': 'alpn', 'v': ev.alpn_protocol})
        if isinstance(ev, HandshakeCompleted):
            REC['events'].append({'t': ms(), 'conn': self._cid, 'ev': 'handshake_completed', 'early_data_accepted': ev.early_data_accepted,
                                  'session_resumed': ev.session_resumed})
            tp = getattr(self._quic, '_remote_transport_parameters', None)
        if isinstance(ev, StreamDataReceived):
            s = REC['streams'].setdefault('%d/%d' % (self._cid, ev.stream_id), {'first_t': ms(), 'hex': ''})
            s['hex'] += ev.data.hex()
            if ev.end_stream:
                s['fin'] = True
        if self._h3 is None:
            return
        for h in self._h3.handle_event(ev):
            if isinstance(h, HeadersReceived):
                hd = [[k.decode(), v.decode()] for k, v in h.headers]
                path = dict(hd).get(':path', '/')
                REC['requests'].append({'t': ms(), 'transport': 'h3', 'conn': self._cid, 'stream_id': h.stream_id, 'headers': hd})
                ctype, body = body_for(path)
                self._h3.send_headers(h.stream_id, [(b':status', b'200'), (b'content-type', ctype.encode()),
                                                    (b'alt-svc', ('h3=":%d"; ma=86400' % PORT).encode()),
                                                    (b'cache-control', b'no-store')])
                self._h3.send_data(h.stream_id, body, end_stream=True)
                if path.startswith('/done'):
                    DONE.set()
        self.transmit()


# ---- the TCP side ----------------------------------------------------------------------------------------------
async def tcp_conn(reader, writer):
    try:
        while True:
            head = await reader.readuntil(b'\r\n\r\n')
            lines = head.decode('latin1').split('\r\n')
            reqline = lines[0]
            hd = [l.split(': ', 1) for l in lines[1:] if l]
            n = int(dict((k.lower(), v) for k, v in hd).get('content-length', '0'))
            if n:
                await reader.readexactly(n)
            path = reqline.split(' ')[1]
            REC['requests'].append({'t': ms(), 'transport': 'tcp', 'line': reqline, 'headers': hd})
            ctype, body = body_for(path)
            writer.write(('HTTP/1.1 200 OK\r\nContent-Type: %s\r\nContent-Length: %d\r\nCache-Control: no-store\r\n'
                          'Alt-Svc: h3=":%d"; ma=86400\r\n\r\n' % (ctype, len(body), PORT)).encode() + body)
            await writer.drain()
            if path.startswith('/done'):
                DONE.set()
    except Exception:
        pass


async def main():
    cfg = QuicConfiguration(is_client=False, alpn_protocols=H3_ALPN, max_datagram_frame_size=65536)
    cfg.load_cert_chain(os.path.join(HERE, 'certs/leaf.pem'), os.path.join(HERE, 'certs/leaf.key'))
    loop = asyncio.get_running_loop()
    await serve('127.0.0.1', PORT, configuration=cfg, create_protocol=H3Rec)
    # `serve` builds a plain QuicServer; put the recording subclass in its place on a second bind is not possible, so
    # the class is swapped on the live transport's protocol.
    sctx = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
    sctx.load_cert_chain(os.path.join(HERE, 'certs/leaf.pem'), os.path.join(HERE, 'certs/leaf.key'))
    sctx.set_alpn_protocols(['http/1.1'])
    await asyncio.start_server(tcp_conn, '127.0.0.1', PORT, ssl=sctx)
    print('ready', PORT, file=sys.stderr, flush=True)
    try:
        await asyncio.wait_for(DONE.wait(), SECS)
        await asyncio.sleep(0.5)
    except asyncio.TimeoutError:
        REC['events'].append({'t': ms(), 'ev': 'timeout'})
    ports = []
    for d in REC['datagrams']:
        if d.get('from') not in ports:
            ports.append(d.get('from'))
    REC['client_hellos'] = [summarize_initial(p) for p in ports]
    REC['client_hello'] = REC['client_hellos'][0] if ports else None
    for d in REC['datagrams']:
        for p in d.get('packets', []):
            for f in p.get('frames', []):
                f.pop('_data', None)
    json.dump(REC, open(OUT, 'w'), indent=1)


_ORIG_DGRAM = QuicServer.datagram_received
QuicServer.datagram_received = RecServer.datagram_received
asyncio.run(main())
