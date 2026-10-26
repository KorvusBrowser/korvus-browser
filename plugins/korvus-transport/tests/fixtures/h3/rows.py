"""rows.py REC.json [REC.json ...]: the fixture rows of a Chrome recording (h3rec.py), computed exactly as
`vendor/wreq/src/http3/testing.rs` computes korvus's. With several recordings, prints the rows only if all agree
(else the differing rows, to stderr, and exit 1).

Run with the recorder's venv: venv/bin/python rows.py rec/force-1.json rec/force-2.json rec/force-3.json
"""
import json, struct, sys

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.primitives import hashes, hmac

V1_SALT = bytes.fromhex('38762cf7f55934b34d179ae6a4c80cadccbb7f0a')


def hkdf_extract(salt, ikm):
    h = hmac.HMAC(salt, hashes.SHA256()); h.update(ikm); return h.finalize()


def expand_label(secret, label, length):
    full = b'tls13 ' + label.encode()
    info = struct.pack('>H', length) + bytes([len(full)]) + full + b'\x00'
    out, t, i = b'', b'', 1
    while len(out) < length:
        h = hmac.HMAC(secret, hashes.SHA256()); h.update(t + info + bytes([i])); t = h.finalize(); out += t; i += 1
    return out[:length]


def varint(b, i):
    if i >= len(b):
        return None, i
    first = b[i]; n = 1 << (first >> 6)
    if i + n > len(b):
        return None, i
    v = first & 0x3f
    for k in range(1, n):
        v = (v << 8) | b[i + k]
    return v, i + n


def frames(p):
    out, i = [], 0
    while i < len(p):
        t = p[i]
        if t == 0:
            s = i
            while i < len(p) and p[i] == 0:
                i += 1
            out.append((0, i - s, b''))
        elif t == 1:
            out.append((1, 0, b'')); i += 1
        elif t == 6:
            i += 1
            off, i = varint(p, i); ln, i = varint(p, i)
            out.append((6, off, p[i:i + ln])); i += ln
        elif t in (2, 3):
            i += 1
            _, i = varint(p, i); _, i = varint(p, i); n, i = varint(p, i); _, i = varint(p, i)
            for _ in range(n):
                _, i = varint(p, i); _, i = varint(p, i)
            if t == 3:
                for _ in range(3):
                    _, i = varint(p, i)
            out.append((2, 0, b''))
        else:
            break
    return out


def initial_packets(d, odcid):
    out, i = [], 0
    while i < len(d) and d[i] & 0x80:
        first = d[i]
        version = struct.unpack('>I', d[i + 1:i + 5])[0]
        j = i + 5
        dl = d[j]; dcid = d[j + 1:j + 1 + dl]; j += 1 + dl
        sl = d[j]; j += 1 + sl
        ty = (first >> 4) & 3
        token_len = 0
        if ty == 0:
            token_len, j = varint(d, j); j += token_len
        ln, j = varint(d, j)
        end = min(j + ln, len(d))
        if ty == 0:
            if odcid[0] is None:
                odcid[0] = dcid
            prk = hkdf_extract(V1_SALT, odcid[0])
            c = expand_label(prk, 'client in', 32)
            key, iv, hp = expand_label(c, 'quic key', 16), expand_label(c, 'quic iv', 12), expand_label(c, 'quic hp', 16)
            pkt = bytearray(d[i:end])
            pn_off = j - i
            if len(pkt) >= pn_off + 20:
                enc = Cipher(algorithms.AES(hp), modes.ECB()).encryptor()
                mask = enc.update(bytes(pkt[pn_off + 4:pn_off + 20])) + enc.finalize()
                pkt[0] ^= mask[0] & 0x0f
                pnl = (pkt[0] & 3) + 1
                for k in range(pnl):
                    pkt[pn_off + k] ^= mask[1 + k]
                pn = int.from_bytes(pkt[pn_off:pn_off + pnl], 'big')
                nonce = (int.from_bytes(iv, 'big') ^ pn).to_bytes(12, 'big')
                try:
                    plain = AESGCM(key).decrypt(nonce, bytes(pkt[pn_off + pnl:]), bytes(pkt[:pn_off + pnl]))
                    out.append(dict(version=version, dcid=dcid, scid_len=sl, token_len=token_len, pn=pn, frames=frames(plain)))
                except Exception:
                    pass
        i = end
    return out


def is_grease16(v):
    return (v & 0x0f0f) == 0x0a0a and (v >> 8) == (v & 0xff)


def hex16(v):
    return 'GREASE' if is_grease16(v) else '%04x' % v


def list16(b):
    return ','.join(hex16(int.from_bytes(b[k:k + 2], 'big')) for k in range(0, len(b) - 1, 2))


def names(b):
    out, i = [], 0
    while i < len(b):
        n = b[i]; out.append(b[i + 1:i + 1 + n].decode('latin1')); i += 1 + n
    return ','.join(out)


def tp_rows(d):
    rows, ids, i = [], [], 0
    while i < len(d):
        pid, i = varint(d, i); ln, i = varint(d, i)
        if pid is None or ln is None:
            break
        v = d[i:i + ln]; i += ln
        if pid % 31 == 27 and pid > 0x3f:
            ids.append('GREASE')
            rows.append('tp.grease_len_le_15=%s' % ('true' if ln <= 15 else 'false'))
            continue
        ids.append(hex(pid))
        if pid in (0x01, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0e, 0x20, 0x3127):
            value = str(varint(v, 0)[0]) if v else ''
        elif pid == 0x0f:
            value = 'len %d' % len(v)
        elif pid == 0x11:
            words = []
            for k in range(0, len(v), 4):
                w = int.from_bytes(v[k:k + 4], 'big')
                words.append('GREASE' if (w & 0x0f0f0f0f) == 0x0a0a0a0a else '%08x' % w)
            chosen, rest = words[0], sorted(words[1:])
            value = '%s|%s' % (chosen, ','.join(rest))
        elif pid == 0x3128:
            value = v.decode('latin1')
        else:
            value = v.hex()
        rows.append('tp.%s=%s' % (hex(pid), value))
    ids.sort()
    rows.append('tp.ids=' + ','.join(ids))
    return rows


def client_hello_rows(ch):
    rows = []
    if len(ch) < 4 or ch[0] != 1:
        return ['ch=missing']
    i = 4
    rows.append('ch.legacy_version=%02x%02x' % (ch[i], ch[i + 1])); i += 2 + 32
    sid = ch[i]; rows.append('ch.session_id_len=%d' % sid); i += 1 + sid
    cl = int.from_bytes(ch[i:i + 2], 'big'); rows.append('ch.ciphers=' + list16(ch[i + 2:i + 2 + cl])); i += 2 + cl
    ml = ch[i]; i += 1 + ml
    el = int.from_bytes(ch[i:i + 2], 'big'); i += 2
    end = min(i + el, len(ch))
    types = []
    while i + 4 <= end:
        t = int.from_bytes(ch[i:i + 2], 'big'); l = int.from_bytes(ch[i + 2:i + 4], 'big')
        d = ch[i + 4:min(i + 4 + l, end)]; i += 4 + l
        types.append(hex16(t))
        if t == 0x0000:
            rows.append('ch.ext.server_name=' + d[5:].decode('latin1'))
        elif t == 0x000a:
            rows.append('ch.ext.supported_groups=' + list16(d[2:]))
        elif t == 0x000d:
            rows.append('ch.ext.signature_algorithms=' + list16(d[2:]))
        elif t == 0x0010:
            rows.append('ch.ext.alpn=' + names(d[2:]))
        elif t == 0x0033:
            k, shares = 2, []
            while k + 4 <= len(d):
                g = int.from_bytes(d[k:k + 2], 'big'); n = int.from_bytes(d[k + 2:k + 4], 'big')
                shares.append('%s:%d' % (hex16(g), n)); k += 4 + n
            rows.append('ch.ext.key_share=' + ','.join(shares))
        elif t == 0x002b:
            rows.append('ch.ext.supported_versions=' + list16(d[1:]))
        elif t == 0x002d:
            rows.append('ch.ext.psk_key_exchange_modes=' + d[1:].hex())
        elif t == 0x001b:
            rows.append('ch.ext.compress_certificate=' + list16(d[1:]))
        elif t in (0x44cd, 0x4469):
            rows.append('ch.ext.application_settings=' + names(d[2:]))
        elif t == 0xfe0d:
            rows.append('ch.ext.encrypted_client_hello.outer=%d' % (d[0] if d else 9))
        elif t == 0x0039:
            rows.extend(tp_rows(d))
    rows.append('ch.extension_set=' + ','.join(sorted(types)))
    rows.append('ch.grease=' + ('true' if 'GREASE' in types else 'false'))
    return rows


def b(x):
    return 'true' if x else 'false'


def initial_rows(datagrams):
    rows = []
    if not datagrams:
        return rows
    port = datagrams[0][0]
    odcid = [None]
    first = None
    crypto, sizes, with_crypto = {}, [], 0
    for p, d in datagrams:
        if p != port:
            continue
        pkts = initial_packets(d, odcid)
        if not pkts:
            continue
        sizes.append(len(d))
        for pk in pkts:
            has_crypto = any(f[0] == 6 for f in pk['frames'])
            if has_crypto and all(f[0] != 2 for f in pk['frames']):
                with_crypto += 1
            for ty, off, data in pk['frames']:
                if ty == 6:
                    crypto[off] = data
            if first is None:
                first = pk
    if first is None:
        return rows
    rows += ['initial.version=%08x' % first['version'], 'initial.dcid_len=%d' % len(first['dcid']),
             'initial.scid_len=%d' % first['scid_len'], 'initial.token_len=%d' % first['token_len'],
             'initial.first_pn=%d' % first['pn'], 'initial.datagram_sizes=' + ','.join(str(s) for s in sorted(set(sizes)))]
    ch, off = b'', 0
    while off in crypto:
        ch += crypto[off]; off += len(crypto[off])
    fr = first['frames']
    rows.append('initial.first_packet.crypto_frames_ge_3=' + b(sum(1 for f in fr if f[0] == 6) >= 3))
    rows.append('initial.first_packet.has_ping=' + b(any(f[0] == 1 for f in fr)))
    head = any(f[0] == 6 and f[1] == 0 for f in fr)
    tail = any(f[0] == 6 and f[1] + len(f[2]) == len(ch) for f in fr)
    rows.append('initial.first_packet.head_and_tail=' + b(head and tail))
    rows.append('initial.client_hello_packets=%d' % with_crypto)
    rows += client_hello_rows(ch)
    return rows


def h3_rows(streams, requests):
    rows, updates = [], []
    uni = []
    for sid in sorted(streams):
        bts = streams[sid]
        if sid % 4 == 2 and bts:
            t, _ = varint(bts, 0)
            uni.append((sid, t))
    rows.append('h3.uni_streams=' + ','.join('%d:%d' % u for u in uni))
    ctl = [s for s, t in uni if t == 0]
    if ctl:
        bts = streams[ctl[0]]
        _, at = varint(bts, 0)
        kinds = []
        while at < len(bts):
            ty, at = varint(bts, at); ln, at = varint(bts, at)
            if ty is None or ln is None:
                break
            p = bts[at:at + ln]; at += ln
            if ty == 4:
                k, s = 0, []
                while k < len(p):
                    i, k = varint(p, k); v, k = varint(p, k)
                    s.append('GREASE' if i >= 0x21 and (i - 0x21) % 0x1f == 0 else '%s=%d' % (hex(i), v))
                rows.append('h3.settings=' + ','.join(s)); kinds.append('SETTINGS')
            elif ty == 0xf0700:
                e, k = varint(p, 0)
                updates.append((e, p[k:].decode('latin1'))); kinds.append('PRIORITY_UPDATE')
            elif ty >= 0x21 and (ty - 0x21) % 0x1f == 0:
                rows.append('h3.grease_frame_len_le_3=' + b(ln <= 3)); kinds.append('GREASE')
            else:
                kinds.append(hex(ty))
        dedup = [k for n, k in enumerate(kinds) if n == 0 or kinds[n - 1] != k]
        rows.append('h3.control_frames=' + ','.join(dedup))
    enc = [s for s, t in uni if t == 2]
    if enc:
        bts = streams[enc[0]]
        _, at = varint(bts, 0)
        if at < len(bts):
            x = bts[at]
            if x & 0xe0 == 0x20:
                cap = x & 0x1f
                if cap == 0x1f:
                    shift, k = 0, at + 1
                    while k < len(bts):
                        y = bts[k]; cap += (y & 0x7f) << shift; shift += 7; k += 1
                        if not y & 0x80:
                            break
                rows.append('h3.qpack_encoder.first=set_capacity:%d' % cap)
            else:
                rows.append('h3.qpack_encoder.first=%#04x' % x)
    sids = sorted(requests)
    for i, sid in enumerate(sids[:1]):
        f = requests[sid]
        rows.append('h3.request%d.pseudo=' % i + ','.join(n for n, _ in f if n.startswith(':')))
        rows.append('h3.request%d.names=' % i + ','.join(n for n, _ in f if not n.startswith(':')))
        pr = [v for n, v in f if n == 'priority']
        if pr:
            rows.append('h3.request%d.priority=%s' % (i, pr[0]))
    rows.append('h3.first_request_stream=%d' % (sids[0] if sids else 2 ** 64 - 1))
    if updates:
        rows.append('h3.priority_update.first=%d:%s' % updates[0])
    expected = sorted((sid, v) for sid in sids for n, v in requests[sid] if n == 'priority' and v != 'u=3')
    rows.append('h3.priority_updates_match_headers=' + b(expected == sorted(updates)))
    return rows


def rows_of(path):
    r = json.load(open(path))
    dg = [(d['from'], bytes.fromhex(d['hex'])) for d in r['datagrams'] if d.get('hex')]
    rows = initial_rows(dg)
    conns = sorted({q['conn'] for q in r['requests'] if q['transport'] == 'h3'})
    if conns:
        c = conns[0]
        streams = {int(k.split('/')[1]): bytes.fromhex(v['hex']) for k, v in r['streams'].items() if int(k.split('/')[0]) == c}
        requests = {q['stream_id']: [tuple(h) for h in q['headers']] for q in r['requests'] if q['transport'] == 'h3' and q['conn'] == c}
        rows += h3_rows(streams, requests)
    return rows


if __name__ == '__main__':
    runs = [sorted(rows_of(p)) for p in sys.argv[1:]]
    base = runs[0]
    bad = False
    for p, rr in zip(sys.argv[1:], runs):
        if rr != base:
            bad = True
            print('DIFFERS: %s' % p, file=sys.stderr)
            for x in sorted(set(rr) ^ set(base)):
                print('   ', x, file=sys.stderr)
    if bad:
        sys.exit(1)
    print(json.dumps(base, indent=1))
